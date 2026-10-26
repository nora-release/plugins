use super::*;
use base64::Engine;
use futures::{stream, StreamExt};
use nora_ai::{HttpClientError, HttpStreamingResponse};
use std::{collections::VecDeque, sync::Mutex};
struct Reply {
    status: u16,
    body: Vec<u8>,
}
impl Reply {
    fn json(value: Value) -> Self {
        Self {
            status: 200,
            body: serde_json::to_vec(&value).unwrap(),
        }
    }
}
struct Client {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<Reply>>,
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected Cloudflare HTTP request");
        Ok(HttpStreamingResponse {
            status: reply.status,
            headers: BTreeMap::new(),
            body: Box::pin(stream::iter([Ok(reply.body.into())])),
        })
    }
}
fn catalog() -> Value {
    json!({ID:{"models":{
        "@cf/meta/llama-3-8b-instruct":{"name":"Llama 3 8B","limit":{"context":8192,"output":4096},"tool_call":true,"cost":{"input":0.05,"output":0.12},"modalities":{"input":["text"],"output":["text"]}},
        "@cf/deepseek/deepseek-r1":{"name":"DeepSeek R1","reasoning":true,"tool_call":true,"reasoning_options":[{"type":"effort","values":["low","high"]}],"modalities":{"input":["text","image"],"output":["text"]}},
        "@cf/zai-org/glm-5.3":{"name":"GLM 5.3","limit":{"context":128000,"output":32000},"modalities":{"input":["text"],"output":["text"]}}
    }}})
}
fn credentials(account: &str) -> Value {
    json!({"type":"api","key":format!("token-{account}"),"accountId":account.repeat(32)})
}
fn context(account: &str) -> CallContext {
    CallContext {
        credentials: Some(credentials(account)),
        ..Default::default()
    }
}
async fn fixture(replies: Vec<Reply>) -> (CloudflareWorkersAi, Arc<Client>) {
    let client = Arc::new(Client {
        requests: Mutex::default(),
        replies: Mutex::new(replies.into()),
    });
    let provider = CloudflareWorkersAi {
        client: Some(client.clone()),
        ..Default::default()
    };
    provider
        .initialize(InitializeParams {
            api_version: API_VERSION,
            subscription_accounts_enabled: true,
            plugin_id: ID.into(),
            plugin_directory: Default::default(),
            data_directory: Default::default(),
            manifest: serde_json::from_str(include_str!("../nora-provider.json")).unwrap(),
            installed_manifest: None,
            runtime_config: Value::Null,
        })
        .await
        .unwrap();
    (provider, client)
}
fn request(account: &str, path: &str, body: &[u8]) -> FetchRequest {
    FetchRequest {
        caller_user_agent: None,
        constraints: Default::default(),
        subscription_accounts_enabled: true,
        account: account.into(),
        credentials: Some(credentials(account)),
        network_proxy: None,
        url: format!(
            "https://api.cloudflare.com/client/v4/accounts/${{CLOUDFLARE_ACCOUNT_ID}}/ai/v1/{path}"
        ),
        method: "POST".into(),
        headers: BTreeMap::from([
            ("authorization".into(), "untrusted-caller".into()),
            ("host".into(), "evil.invalid".into()),
            ("content-type".into(), "application/json".into()),
            ("cf-aig-metadata".into(), "{}".into()),
        ]),
        metadata: Default::default(),
        body: Some(base64::engine::general_purpose::STANDARD.encode(body)),
        body_file: None,
    }
}
#[tokio::test]
async fn initialization_is_lazy_and_disconnected_catalog_does_not_make_requests() {
    let (provider, client) = fixture(vec![]).await;
    assert_eq!(
        provider.models(CallContext::default()).await.unwrap()["source"],
        "preset"
    );
    assert_eq!(
        provider.config().unwrap().manifest["config"]["maxConcurrency"],
        3
    );
    assert!(client.requests.lock().unwrap().is_empty());
    assert_eq!(
        provider.auth_methods(Default::default()).await.unwrap()[0]["fields"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}
#[tokio::test]
async fn account_identity_replaces_the_same_account_when_its_token_is_rotated() {
    let (provider, _) = fixture(vec![]).await;
    let one = provider
        .authorize(json!({"method":"apiKey","inputs":{"token":"old","accountId":"a".repeat(32)}}))
        .await
        .unwrap();
    let two = provider
        .authorize(json!({"method":"apiKey","inputs":{"apiKey":"new","accountId":"A".repeat(32)}}))
        .await
        .unwrap();
    assert_eq!(
        one["credentials"]["accountIdentity"],
        two["credentials"]["accountIdentity"]
    );
    assert_ne!(one["credentials"]["key"], two["credentials"]["key"]);
    for account in ["", "../escape", "a", "g1234567890123456789012345678901"] {
        assert!(provider
            .authorize(json!({"inputs":{"token":"tok","accountId":account}}))
            .await
            .is_err());
    }
    assert!(provider
        .authorize(json!({"inputs":{"accountId":"a".repeat(32)}}))
        .await
        .is_err());
}
#[tokio::test]
async fn per_account_endpoint_and_private_token_override_template_and_public_headers() {
    let (provider, client) = fixture(vec![
        Reply::json(json!({"ok":true})),
        Reply::json(json!({"ok":true})),
    ])
    .await;
    for account in ["a", "b"] {
        provider
            .fetch(request(
                account,
                "chat/completions?x=1",
                br#"{"model":"@cf/meta/llama-3-8b-instruct","messages":[]}"#,
            ))
            .await
            .unwrap();
    }
    let requests = client.requests.lock().unwrap();
    for (index, account) in ["a", "b"].into_iter().enumerate() {
        assert_eq!(
            requests[index].url,
            format!(
                "https://api.cloudflare.com/client/v4/accounts/{}/ai/v1/chat/completions?x=1",
                account.repeat(32)
            )
        );
        assert_eq!(
            requests[index].headers["authorization"],
            format!("Bearer token-{account}")
        );
        assert!(!requests[index].headers.contains_key("host"));
        assert_eq!(requests[index].headers["cf-aig-metadata"], "{}");
    }
}
#[tokio::test]
async fn discovery_keeps_vendor_capabilities_marks_paid_models_and_never_marks_daily_allocation_free(
) {
    let (provider, _) = fixture(vec![Reply::json(catalog())]).await;
    let models = provider.models(context("a")).await.unwrap();
    let rows = models["models"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    for model in rows {
        assert_eq!(model["info"]["free"], false);
        assert!(model["baseUrl"].as_str().unwrap().contains(&"a".repeat(32)));
    }
    let paid = rows
        .iter()
        .find(|m| m["id"] == "@cf/zai-org/glm-5.3")
        .unwrap();
    assert!(paid["name"].as_str().unwrap().ends_with(" (Paid)"));
    let thinking = rows
        .iter()
        .find(|m| m["id"] == "@cf/deepseek/deepseek-r1")
        .unwrap();
    assert_eq!(thinking["reasoning"], true);
    assert_eq!(thinking["contextWindow"], 32768);
}
#[tokio::test]
async fn failed_catalog_refresh_keeps_last_good_models_and_chat_never_refreshes_discovery() {
    let (provider, client) = fixture(vec![
        Reply::json(catalog()),
        Reply {
            status: 503,
            body: b"down".to_vec(),
        },
        Reply::json(json!({"ok":true})),
    ])
    .await;
    provider.config.write().unwrap().as_mut().unwrap().ttl = 0;
    provider.models(context("a")).await.unwrap();
    let cached = provider.models(context("a")).await.unwrap();
    assert_eq!(cached["fellBack"], true);
    assert!(provider
        .fetch(request("a", "chat/completions", br#"{"model":"@cf/nope"}"#))
        .await
        .is_err());
    provider
        .fetch(request(
            "a",
            "chat/completions",
            br#"{"model":"@cf/meta/llama-3-8b-instruct"}"#,
        ))
        .await
        .unwrap();
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests.iter().filter(|r| r.url == CATALOG).count(), 2);
}
#[tokio::test]
async fn an_unprimed_catalog_never_blocks_a_chat_and_embeddings_keep_their_body() {
    let (provider, client) = fixture(vec![Reply::json(json!({"ok":true}))]).await;
    let original = br#"{ "model":"unknown", "input":"embed this" }"#;
    provider
        .fetch(request("a", "embeddings", original))
        .await
        .unwrap();
    assert_eq!(
        client.requests.lock().unwrap()[0].body.as_deref(),
        Some(original.as_slice())
    );
}
#[tokio::test]
async fn usage_has_unknown_consumption_and_next_midnight_without_inventing_a_balance() {
    let (provider, _) = fixture(vec![Reply::json(
        json!({"result":{"plan":{"name":"Workers Free"}}}),
    )])
    .await;
    let usage = provider.usage(context("a")).await.unwrap();
    assert_eq!(usage["plan"], "Workers Free");
    let item = &usage["items"][0];
    assert_eq!(item["limit"], 10000);
    assert_eq!(item["windowSeconds"], 86400);
    assert_eq!(item["countsForRouting"], false);
    for field in ["used", "remaining", "usedPercent"] {
        assert!(item.get(field).is_none());
    }
    assert!(usage["balances"].as_array().unwrap().is_empty());
    assert!(item["resetsAt"].as_str().unwrap().contains("T00:00:00"));
}
#[tokio::test]
async fn invalid_accounts_never_reach_cloudflare_and_plan_permission_failures_keep_the_allowance() {
    let (provider, client) = fixture(vec![Reply {
        status: 403,
        body: Vec::new(),
    }])
    .await;
    let usage = provider.usage(context("a")).await.unwrap();
    assert_eq!(usage["items"].as_array().unwrap().len(), 1);
    assert!(provider
        .usage(CallContext {
            credentials: Some(json!({"key":"tok","accountId":"../escape"})),
            ..Default::default()
        })
        .await
        .unwrap()["items"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn custom_base_uses_the_selected_account_and_rejects_credentials_or_non_http_templates() {
    let (provider, _) = fixture(vec![]).await;
    provider.config.write().unwrap().as_mut().unwrap().template =
        "https://custom.invalid/accounts/${CLOUDFLARE_ACCOUNT_ID}/ai/v1".into();
    let (outgoing, _) = provider
        .outgoing(&request("b", "embeddings", b"{}"))
        .await
        .unwrap();
    assert!(outgoing.url.starts_with(&format!(
        "https://custom.invalid/accounts/{}/",
        "b".repeat(32)
    )));
    for value in [
        "javascript:alert(1)",
        "https://token@custom.invalid/v1",
        "ftp://custom.invalid/v1",
    ] {
        assert!(checked_base(value).is_err());
    }
}
#[tokio::test]
async fn a_refused_api_token_is_an_authentication_error_and_response_bodies_remain_streamed() {
    let (provider, _) = fixture(vec![Reply {
        status: 401,
        body: Vec::new(),
    }])
    .await;
    assert_eq!(
        provider
            .fetch(request("a", "chat/completions", b"{}"))
            .await
            .err()
            .unwrap()
            .code,
        "authentication_required"
    );
    let (provider, _) = fixture(vec![Reply {
        status: 200,
        body: b"data: hello\n\n".to_vec(),
    }])
    .await;
    let mut response = provider
        .fetch(request("a", "chat/completions", b"{}"))
        .await
        .unwrap();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.body.next().await {
        bytes.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(bytes, b"data: hello\n\n");
}

#[tokio::test]
async fn a_fresh_plugin_process_reuses_the_catalog_without_http_and_rebinds_account_base() {
    let directory = std::env::temp_dir().join(format!(
        "nora-cloudflare-cache-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap()
    ));
    let (provider, _) = fixture(vec![Reply::json(catalog())]).await;
    provider.config.write().unwrap().as_mut().unwrap().directory = directory.clone();
    provider.models(context("a")).await.unwrap();
    drop(provider);
    let (provider, client) = fixture(vec![]).await;
    provider.config.write().unwrap().as_mut().unwrap().directory = directory.clone();
    let models = provider.models(context("b")).await.unwrap();
    assert_eq!(models["models"].as_array().unwrap().len(), 3);
    assert!(models["models"][0]["baseUrl"]
        .as_str()
        .unwrap()
        .contains(&"b".repeat(32)));
    assert!(client.requests.lock().unwrap().is_empty());
    std::fs::remove_dir_all(directory).unwrap();
}
#[tokio::test]
async fn verified_body_files_are_forwarded_byte_for_byte_and_changed_files_are_refused() {
    let (provider, client) = fixture(vec![Reply::json(json!({"ok":true}))]).await;
    let path = std::env::temp_dir().join(format!(
        "nora-cloudflare-body-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap()
    ));
    let original = br#"{ "model":"arbitrary", "messages":[] }"#;
    std::fs::write(&path, original).unwrap();
    let mut req = request("a", "chat/completions", b"");
    req.body = None;
    req.body_file = Some(nora_provider_plugin_sdk::RequestBodyFile::from_path(&path).unwrap());
    provider.fetch(req.clone()).await.unwrap();
    assert_eq!(
        client.requests.lock().unwrap()[0].body.as_deref(),
        Some(original.as_slice())
    );
    std::fs::write(&path, b"changed").unwrap();
    assert!(provider.fetch(req).await.is_err());
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn legacy_two_field_accounts_keep_their_token_and_account_id() {
    let (provider, client) = fixture(vec![Reply::json(json!({"ok":true}))]).await;
    let mut req = request("a", "chat/completions", b"{}");
    req.credentials =
        Some(json!({"type":"api","methods":[{"key":"legacy-token"},{"key":"B".repeat(32)}]}));
    provider.fetch(req).await.unwrap();
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].headers["authorization"], "Bearer legacy-token");
    assert!(requests[0].url.contains(&"b".repeat(32)));
}
