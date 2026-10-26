use super::*;
use base64::Engine;
use nora_ai::HttpClientError;
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex as StdMutex,
    },
};
struct Reply {
    path: &'static str,
    status: u16,
    kind: &'static str,
    bytes: Vec<u8>,
}
fn reply(path: &'static str, status: u16, value: Value) -> Reply {
    Reply {
        path,
        status,
        kind: "application/json",
        bytes: serde_json::to_vec(&value).unwrap(),
    }
}
fn generated() -> Reply {
    let mut bytes = Vec::new();
    for value in [
        json!({"type":"text-delta","text":"Real reply"}),
        json!({"type":"finish","finishReason":"stop","totalUsage":{"inputTokens":11,"outputTokens":2}}),
    ] {
        bytes.extend(serde_json::to_vec(&value).unwrap());
        bytes.push(b'\n');
    }
    Reply {
        path: "/alpha/generate",
        status: 200,
        kind: "application/x-ndjson",
        bytes,
    }
}
struct Client {
    requests: StdMutex<Vec<HttpRequest>>,
    replies: StdMutex<VecDeque<Reply>>,
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        let mut replies = self.replies.lock().unwrap();
        let index = replies
            .iter()
            .position(|reply| request.url.ends_with(reply.path))
            .unwrap_or_else(|| panic!("Unexpected Command Code request: {}", request.url));
        let reply = replies.remove(index).unwrap();
        drop(replies);
        self.requests.lock().unwrap().push(request);
        Ok(HttpStreamingResponse {
            status: reply.status,
            headers: BTreeMap::from([("content-type".into(), reply.kind.into())]),
            body: Box::pin(futures::stream::once(async move { Ok(reply.bytes.into()) })),
        })
    }
}
struct Store {
    saved: StdMutex<Vec<Value>>,
    failures: AtomicUsize,
}
#[async_trait]
impl core::CredentialStore for Store {
    async fn save(&self, value: Value) -> PluginResult<()> {
        self.saved.lock().unwrap().push(value);
        if self
            .failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
                count.checked_sub(1)
            })
            .is_ok()
        {
            return Err(PluginError::transport("Save failed"));
        }
        Ok(())
    }
}
fn setup(replies: Vec<Reply>) -> (CommandCode, Arc<Client>, Arc<Store>) {
    let client = Arc::new(Client {
        requests: StdMutex::new(Vec::new()),
        replies: StdMutex::new(replies.into()),
    });
    let store = Arc::new(Store {
        saved: StdMutex::new(Vec::new()),
        failures: AtomicUsize::new(0),
    });
    let core = Core::with_dependencies("commandcode-plan", client.clone(), store.clone());
    core.initialize(InitializeParams {
        subscription_accounts_enabled: true,
        installed_manifest: None,
        runtime_config: serde_json::Value::Null,
        api_version: 1,
        plugin_id: "commandcode-plan".into(),
        plugin_directory: PathBuf::new(),
        data_directory: PathBuf::new(),
        manifest: serde_json::from_str(include_str!("../nora-provider.json")).unwrap(),
    })
    .unwrap();
    (
        CommandCode {
            core,
            browsers: Mutex::new(BTreeMap::new()),
            plans: Mutex::new(BTreeMap::new()),
            session: "fallback-session".into(),
        },
        client,
        store,
    )
}
fn credentials() -> Value {
    let mut value = auth::credential(&json!({"apiKey":"real-key"}), "apiKey").unwrap();
    value["uid"] = json!("uid-one");
    value["accountId"] = json!("uid-one");
    value
}
fn fetch_request(credentials: Value, body: Value) -> FetchRequest {
    FetchRequest {
        caller_user_agent: None,
        constraints: Default::default(),
        subscription_accounts_enabled: true,
        account: "one".into(),
        credentials: Some(credentials),
        network_proxy: None,
        url: format!("{}/chat/completions", catalog::BASE),
        method: "POST".into(),
        headers: BTreeMap::new(),
        metadata: BTreeMap::from([("sessionID".into(), "private-host-session".into())]),
        body: Some(
            base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&body).unwrap()),
        ),
        body_file: None,
    }
}
async fn cached_plan(plugin: &CommandCode, plan: &str) {
    plugin.plans.lock().await.insert(
        core::fingerprint(&json!("real-key")),
        (
            core::now(),
            json!({"plan":plan,"id":format!("individual-{}",plan.to_lowercase())}),
        ),
    );
}
async fn body(response: FetchResponse) -> Value {
    let bytes = response
        .body
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<PluginResult<Vec<_>>>()
        .unwrap()
        .concat();
    serde_json::from_slice(&bytes).unwrap()
}
#[tokio::test]
async fn manual_keys_are_validated_with_whoami_and_keep_the_actual_plan() {
    let (plugin, client, _) = setup(vec![
        reply(
            "/alpha/whoami",
            200,
            json!({"user":{"id":"uid-one","userName":"User","email":"user@example.com"}}),
        ),
        reply(
            "/alpha/billing/subscriptions",
            200,
            json!({"data":{"planId":"individual-go-monthly","status":"active"}}),
        ),
    ]);
    let result = plugin
        .authorize(json!({"authMethodID":"apiKey","inputs":{"key":"real-key"}}))
        .await
        .unwrap();
    assert_eq!(result["credentials"]["uid"], "uid-one");
    assert_eq!(result["credentials"]["plan"], "Go");
    assert_eq!(result["credentials"]["expires"], 0);
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn rejected_keys_never_produce_a_successful_login() {
    let (plugin, _, store) = setup(vec![reply(
        "/alpha/whoami",
        401,
        json!({"error":"invalid"}),
    )]);
    assert!(plugin
        .authorize(json!({"authMethodID":"apiKey","inputs":{"key":"bad"}}))
        .await
        .is_err());
    assert!(store.saved.lock().unwrap().is_empty());
}
#[tokio::test]
async fn cli_sources_never_copy_keys_into_saved_credentials_or_follow_another_user() {
    let path = std::env::temp_dir().join(format!("commandcode-provider-{}.json", auth::state()));
    let original = br#"{"apiKey":"real-key"}"#;
    std::fs::write(&path, original).unwrap();
    let (plugin, client, _) = setup(vec![
        reply("/alpha/whoami", 200, json!({"user":{"id":"uid-one"}})),
        reply("/alpha/billing/subscriptions", 200, json!({"data":null})),
        reply("/alpha/whoami", 200, json!({"user":{"id":"uid-two"}})),
    ]);
    let result = plugin
        .authorize(json!({"authMethodID":"cli","inputs":{"sourcePath":path}}))
        .await
        .unwrap();
    let saved = &result["credentials"];
    assert!(saved.get("key").is_none());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    std::fs::write(&path, br#"{"apiKey":"different-key"}"#).unwrap();
    assert!(plugin.live(saved, client.as_ref()).await.is_err());
    std::fs::remove_file(path).unwrap();
}
#[tokio::test]
async fn known_go_accounts_use_generate_and_preserve_tools_and_session_privacy() {
    let (plugin, client, _) = setup(vec![generated()]);
    cached_plan(&plugin, "Go").await;
    let response=plugin.fetch(fetch_request(credentials(),json!({"model":"gpt-6-luna","stream":false,"messages":[{"role":"user","content":"hello"}],"tools":[{"type":"function","function":{"name":"read","parameters":{"type":"object"}}}]}))).await.unwrap();
    let value = body(response).await;
    assert_eq!(value["choices"][0]["message"]["content"], "Real reply");
    assert_eq!(value["usage"]["prompt_tokens"], 11);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].url.ends_with("/alpha/generate"));
    let sent: Value = serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(sent["params"]["tools"][0]["name"], "read");
    assert_ne!(requests[0].headers["x-session-id"], "private-host-session");
    assert_eq!(requests[0].headers["x-command-code-version"], "1.72.2");
}
#[tokio::test]
async fn a_real_go_api_refusal_is_saved_before_retrying_the_same_request_on_generate() {
    let (plugin, client, store) = setup(vec![
        reply(
            "/provider/v1/chat/completions",
            403,
            json!({"error":{"message":"Your Go plan doesn't include API access. Upgrade to Provider or higher"}}),
        ),
        generated(),
    ]);
    cached_plan(&plugin, "Max").await;
    let response = plugin
        .fetch(fetch_request(
            credentials(),
            json!({"model":"gpt-6-luna","messages":[{"role":"user","content":"hello"}]}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(body(response).await["usage"]["total_tokens"], 13);
    assert_eq!(store.saved.lock().unwrap()[0]["plan"], "Go");
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_go_refusal_changes_subsequent_models_and_routing_even_with_the_old_credentials() {
    let (plugin, client, store) = setup(vec![
        reply(
            "/provider/v1/chat/completions",
            403,
            json!({"error":{"message":"Your Go plan doesn't include API access. Upgrade to Provider or higher"}}),
        ),
        generated(),
        reply(
            "/alpha/billing/credits",
            503,
            json!({"error":"temporarily unavailable"}),
        ),
        reply(
            "/provider/v1/models",
            500,
            json!({"error":"temporarily unavailable"}),
        ),
        generated(),
    ]);
    cached_plan(&plugin, "Max").await;
    let original = credentials();
    let chat = json!({"model":"gpt-6-luna","messages":[{"role":"user","content":"hello"}]});
    assert_eq!(
        body(
            plugin
                .fetch(fetch_request(original.clone(), chat.clone()))
                .await
                .unwrap()
        )
        .await["choices"][0]["message"]["content"],
        "Real reply"
    );
    let models = plugin
        .models(CallContext {
            subscription_accounts_enabled: true,
            account: "one".into(),
            credentials: Some(original.clone()),
            network_proxy: None,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(models["models"], json!(catalog::presets("Go", false)));
    assert_eq!(models["source"], "preset");
    assert!(!models["models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|model| model["id"] == "claude-opus-5-5"));
    assert_eq!(
        body(plugin.fetch(fetch_request(original, chat)).await.unwrap()).await["choices"][0]
            ["message"]["content"],
        "Real reply"
    );
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.url.ends_with("/provider/v1/chat/completions"))
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.url.ends_with("/alpha/generate"))
            .count(),
        2
    );
    assert!(!requests
        .iter()
        .any(|request| request.url.ends_with("/alpha/billing/subscriptions")));
    let catalog = requests
        .iter()
        .find(|request| request.url.ends_with("/models"))
        .unwrap();
    assert!(!catalog.headers.contains_key("authorization"));
    assert!(!catalog.headers.contains_key("x-api-key"));
    let saved = store.saved.lock().unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0]["plan"], "Go");
}
#[tokio::test]
async fn paid_provider_requests_keep_the_original_tool_and_sampling_body() {
    let (plugin, client, _) = setup(vec![reply(
        "/provider/v1/chat/completions",
        200,
        json!({"choices":[{"message":{"content":"yes"}}]}),
    )]);
    cached_plan(&plugin, "Max").await;
    let chat = json!({"model":"native-id","messages":[],"top_p":0.4,"stop":["END"],"tools":[{"type":"function","function":{"name":"read"}}]});
    let request = fetch_request(credentials(), chat.clone());
    let original = request.decode_body().unwrap().unwrap();
    let _ = body(plugin.fetch(request).await.unwrap()).await;
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].body.as_ref().unwrap(), &original);
    assert_eq!(requests[0].headers["x-api-key"], "real-key");
    assert_eq!(requests[0].headers["authorization"], "Bearer real-key");
}
#[tokio::test]
async fn live_go_catalog_uses_no_key_and_keeps_real_entitlements() {
    let (plugin, client, _) = setup(vec![
        reply(
            "/alpha/billing/credits",
            200,
            json!({"credits":{"purchasedCredits":0}}),
        ),
        reply(
            "/provider/v1/models",
            200,
            json!({"data":[{"id":"gpt-6-luna","context_length":1050000},{"id":"claude-opus-5-5"}]}),
        ),
    ]);
    cached_plan(&plugin, "Go").await;
    let value = plugin
        .models(CallContext {
            subscription_accounts_enabled: true,
            account: "one".into(),
            credentials: Some(credentials()),
            network_proxy: None,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(value["source"], "remote");
    assert_eq!(value["models"].as_array().unwrap().len(), 1);
    let requests = client.requests.lock().unwrap();
    let models = requests
        .iter()
        .find(|request| request.url.ends_with("/models"))
        .unwrap();
    assert!(!models.headers.contains_key("authorization"));
    assert!(!models.headers.contains_key("x-api-key"));
}
#[tokio::test]
async fn actual_usage_is_saved_but_temporary_credit_outages_keep_the_account() {
    let (plugin, _, store) = setup(vec![
        reply(
            "/alpha/billing/subscriptions",
            200,
            json!({"data":{"planId":"individual-go-monthly","status":"active"}}),
        ),
        reply(
            "/alpha/billing/credits",
            503,
            json!({"error":"unavailable"}),
        ),
    ]);
    let value = plugin
        .usage(CallContext {
            subscription_accounts_enabled: true,
            account: "one".into(),
            credentials: Some(credentials()),
            network_proxy: None,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(value["available"], false);
    assert_eq!(value["plan"], "Go");
    assert_eq!(store.saved.lock().unwrap()[0]["plan"], "Go");
}
#[test]
fn zero_data_retention_is_opt_in_and_applies_to_both_protocols() {
    let (plugin, _, _) = setup(vec![]);
    let request = fetch_request(credentials(), json!({}));
    assert!(!plugin
        .headers(&credentials(), &request, true)
        .unwrap()
        .contains_key("x-cmd-zdr"));
    let mut manifest: Value = serde_json::from_str(include_str!("../nora-provider.json")).unwrap();
    manifest["config"]["zdr"] = json!(true);
    plugin
        .core
        .initialize(InitializeParams {
            subscription_accounts_enabled: true,
            installed_manifest: None,
            runtime_config: serde_json::Value::Null,
            api_version: 1,
            plugin_id: "commandcode-plan".into(),
            plugin_directory: PathBuf::new(),
            data_directory: PathBuf::new(),
            manifest,
        })
        .unwrap();
    for go in [false, true] {
        assert_eq!(
            plugin.headers(&credentials(), &request, go).unwrap()["x-cmd-zdr"],
            "1"
        );
    }
}
