use super::*;
use crate::fixture::{self, Client, Reply};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;
#[derive(Default)]
struct Store {
    values: Mutex<Vec<Value>>,
    deny: AtomicBool,
}
#[async_trait]
impl core::CredentialStore for Store {
    async fn save(&self, value: Value) -> PluginResult<()> {
        self.values.lock().await.push(value);
        if self.deny.load(Ordering::SeqCst) {
            Err(PluginError::transport("Host refused credential storage."))
        } else {
            Ok(())
        }
    }
}
fn initialize() -> InitializeParams {
    InitializeParams {
        subscription_accounts_enabled: true,
        installed_manifest: None,
        runtime_config: serde_json::Value::Null,
        api_version: 2,
        plugin_id: "factory".into(),
        plugin_directory: std::env::temp_dir(),
        data_directory: std::env::temp_dir(),
        manifest: serde_json::from_str(include_str!("../nora-plugin.json")).unwrap(),
    }
}
async fn plugin(client: Arc<Client>, store: Arc<Store>) -> Factory {
    let p = Factory {
        core: Core::with_dependencies("factory", client.clone(), store),
        client: Some(client),
        ..Default::default()
    };
    p.initialize(initialize()).await.unwrap();
    p
}
fn anthro() -> FetchRequest {
    fixture::fetch(
        "/api/llm/a/v1/messages",
        json!({"model":"claude-opus-5-5","system":"caller instructions","messages":[{"role":"user","content":"actual question"}]}),
    )
}
#[tokio::test]
async fn initialization_does_not_import_install_or_contact_accounts() {
    let client = Client::new(vec![]);
    let p = plugin(client.clone(), Arc::new(Store::default())).await;
    assert_eq!(
        p.auth_methods(CallContext::default())
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(p.accounts(CallContext::default()).await.unwrap(), json!([]));
    assert_eq!(
        p.models(CallContext::default()).await.unwrap()["source"],
        "preset"
    );
    assert!(client.requests.lock().unwrap().is_empty());
    let mut bad = initialize();
    bad.manifest["contributes"]["provider"]["baseURL"] = json!("https://custom.test/v1");
    assert!(p.initialize(bad).await.is_err());
}
#[tokio::test]
async fn native_fetch_sends_factory_auth_and_original_task_without_client_secrets() {
    let client = Client::new(vec![Reply::json(
        json!({"id":"m","content":[{"type":"text","text":"answer"}],"usage":{"input_tokens":5,"output_tokens":2}}),
    )]);
    let p = plugin(client.clone(), Arc::new(Store::default())).await;
    let response = p.fetch(anthro()).await.unwrap();
    assert_eq!(response.status, 200);
    assert!(fixture::body(response).await.contains("input_tokens"));
    let requests = client.requests.lock().unwrap();
    let r = &requests[0];
    assert_eq!(r.url, "https://api.factory.ai/api/llm/a/v1/messages");
    assert_eq!(r.headers["authorization"], "Bearer private-old-access");
    assert_eq!(r.headers["x-api-provider"], "anthropic");
    assert_eq!(r.headers["x-api-key"], "placeholder");
    assert_eq!(r.headers["x-factory-org-id"], "factory-one");
    assert!(!r.headers.contains_key("cookie"));
    let body: Value = serde_json::from_slice(r.body.as_ref().unwrap()).unwrap();
    assert_eq!(body["messages"][0]["content"], "actual question");
    assert_eq!(body["system"][1]["text"], "caller instructions");
}
#[tokio::test]
async fn explicit_api_key_is_verified_and_never_sent_with_an_org_header() {
    let client = Client::new(vec![
        Reply::json(fixture::me()),
        Reply::json(json!({"content":[]})),
    ]);
    let p = plugin(client.clone(), Arc::new(Store::default())).await;
    let result = p
        .authorize(json!({"method":"apiKey","inputs":{"apiKey":"fk-explicit-key"}}))
        .await
        .unwrap();
    assert_eq!(result["credentials"]["accountId"], "user-one");
    let mut request = anthro();
    request.credentials = Some(result["credentials"].clone());
    p.fetch(request).await.unwrap();
    let requests = client.requests.lock().unwrap();
    for request in requests.iter() {
        assert!(!request.headers.contains_key("x-factory-org-id"));
        assert_eq!(request.headers["authorization"], "Bearer fk-explicit-key");
    }
}
#[tokio::test]
async fn rotated_tokens_are_not_used_before_host_ack_and_survive_rejected_save() {
    let client = Client::new(vec![
        Reply::json(fixture::token()),
        Reply::json(fixture::me()),
        Reply::json(json!({"content":[]})),
    ]);
    let store = Arc::new(Store::default());
    store.deny.store(true, Ordering::SeqCst);
    let p = plugin(client.clone(), store.clone()).await;
    let mut request = anthro();
    request.credentials.as_mut().unwrap()["expires"] = json!(1);
    let retry = request.clone();
    assert!(p.fetch(request).await.is_err());
    assert_eq!(client.requests.lock().unwrap().len(), 2);
    store.deny.store(false, Ordering::SeqCst);
    assert_eq!(p.fetch(retry).await.unwrap().status, 200);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[2].headers["authorization"].starts_with("Bearer e30."));
    assert_eq!(
        store.values.lock().await.last().unwrap()["refresh"],
        "private-new-refresh"
    );
}
#[tokio::test]
async fn already_saved_rotation_is_not_spent_twice_by_explicit_refresh() {
    let client = Client::new(vec![
        Reply::json(fixture::token()),
        Reply::json(fixture::me()),
    ]);
    let p = plugin(client.clone(), Arc::new(Store::default())).await;
    let params = json!({"credentials":fixture::saved()});
    p.refresh(params.clone()).await.unwrap();
    p.refresh(params).await.unwrap();
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn org_refusal_repairs_once_before_any_output_and_waits_for_ack() {
    let mut who = fixture::me();
    who["orgId"] = json!("factory-new");
    let client = Client::new(vec![
        Reply::status(
            403,
            json!({"message":"Requested active organization is not accessible by this user"}),
        ),
        Reply::json(who),
        Reply::json(json!({"content":[]})),
    ]);
    let store = Arc::new(Store::default());
    let p = plugin(client.clone(), store.clone()).await;
    assert_eq!(p.fetch(anthro()).await.unwrap().status, 200);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(!requests[1].headers.contains_key("x-factory-org-id"));
    assert_eq!(requests[2].headers["x-factory-org-id"], "factory-new");
    assert_eq!(
        store.values.lock().await[0]["activeOrganizationId"],
        "factory-new"
    );
}
#[tokio::test]
async fn policy_refusal_and_upstream_errors_are_not_replayed_or_reflected() {
    for (status, value) in [
        (
            403,
            json!({"code":"content_policy_rejected","message":"private prompt private token"}),
        ),
        (401, json!({"message":"private prompt"})),
        (413, json!({"message":"private prompt"})),
    ] {
        let client = Client::new(vec![Reply::status(status, value)]);
        let p = plugin(client.clone(), Arc::new(Store::default())).await;
        let response = p.fetch(anthro()).await.unwrap();
        assert_eq!(response.status, status);
        assert!(!fixture::body(response).await.contains("private"));
        assert_eq!(client.requests.lock().unwrap().len(), 1);
    }
}
#[tokio::test]
async fn region_on_prem_and_native_gemini_route_are_applied_without_url_key() {
    let row = models::registry()
        .into_iter()
        .find(|m| models::wire(m) == Some(models::Wire::Gemini))
        .unwrap();
    let model = row["id"].as_str().unwrap();
    let client = Client::new(vec![Reply::json(json!({"candidates":[]}))]);
    let p = plugin(client.clone(), Arc::new(Store::default())).await;
    let mut request = fixture::fetch(
        &format!("/api/llm/g/v1/models/{model}:generateContent?key=private-placeholder"),
        json!({"contents":[]}),
    );
    request.credentials.as_mut().unwrap()["region"] = json!("eu");
    request.credentials.as_mut().unwrap()["premBaseHost"] = json!("models.enterprise.example");
    p.fetch(request).await.unwrap();
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        requests[0].url,
        "https://models.enterprise.example/api/llm/g/v1/generate"
    );
    assert!(!requests[0].headers.contains_key("x-goog-api-key"));
    let body: Value = serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(body["model"], model);
}
#[tokio::test]
async fn distinct_accounts_and_sessions_do_not_share_upstream_session_ids() {
    let client = Client::new(vec![
        Reply::json(json!({})),
        Reply::json(json!({})),
        Reply::json(json!({})),
    ]);
    let p = plugin(client.clone(), Arc::new(Store::default())).await;
    let mut one = anthro();
    one.metadata
        .insert("sessionID".into(), "host-private-session".into());
    let mut two = one.clone();
    two.credentials.as_mut().unwrap()["userId"] = json!("user-two");
    let again = one.clone();
    p.fetch(one).await.unwrap();
    p.fetch(two).await.unwrap();
    p.fetch(again).await.unwrap();
    let r = client.requests.lock().unwrap();
    assert_ne!(r[0].headers["x-session-id"], r[1].headers["x-session-id"]);
    assert_eq!(r[0].headers["x-session-id"], r[2].headers["x-session-id"]);
    assert!(!r[0].headers["x-session-id"].contains("host-private"));
}
#[tokio::test]
async fn api_key_usage_queries_real_billing_limits() {
    let client = Client::new(vec![Reply::json(
        json!({"limits":{"standard":{"fiveHour":{"usedPercent":42,"windowEnd":"2030-01-01T00:00:00Z"}}}}),
    )]);
    let p = plugin(client.clone(), Arc::new(Store::default())).await;
    let result = p
        .usage(CallContext {
            subscription_accounts_enabled: true,
            account: "one".into(),
            credentials: Some(fixture::key()),
            network_proxy: None,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(result["items"][0]["usedPercent"], 42.0);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].url, "https://api.factory.ai/api/billing/limits");
    assert!(!requests[0].headers.contains_key("x-factory-org-id"));
}

fn with_attachments() -> FetchRequest {
    let image = |data: &str| json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":data}});
    fixture::fetch(
        "/api/llm/a/v1/messages",
        json!({
            "model":"claude-opus-5-5","max_tokens":1024,"messages":[
                {"role":"user","content":[{"type":"text","text":"look"},image("older")]},
                {"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"view_image","input":{"path":"/a.png"}}]},
                {"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":[image("middle")]}]},
                {"role":"assistant","content":[{"type":"tool_use","id":"t2","name":"view_image","input":{"path":"/b.png"}}]},
                {"role":"user","content":[{"type":"tool_result","tool_use_id":"t2","content":[{"type":"text","text":"b.png"},image("newest")]}]}
            ]
        }),
    )
}

#[tokio::test]
async fn byte_limit_retries_once_with_older_attachments_replaced_and_tool_pairs_intact() {
    let client = Client::new(vec![
        Reply::status(413, json!({})),
        Reply::json(json!({"content":[]})),
    ]);
    let p = plugin(client.clone(), Arc::new(Store::default())).await;
    let mut original = with_attachments();
    original
        .headers
        .insert("content-length".into(), "99999".into());
    let original_body = original.body.clone();
    assert_eq!(p.fetch(original.clone()).await.unwrap().status, 200);
    assert_eq!(original.body, original_body);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let first: Value = serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
    let next: Value = serde_json::from_slice(requests[1].body.as_ref().unwrap()).unwrap();
    assert_eq!(first["messages"][0]["content"][1]["type"], "image");
    assert_eq!(next["messages"][0]["content"][1]["type"], "text");
    assert!(next["messages"][0]["content"][1]["text"]
        .as_str()
        .unwrap()
        .contains("left out"));
    assert_eq!(
        next["messages"][2]["content"][0]["content"][0],
        next["messages"][0]["content"][1]
    );
    for i in [1, 3, 4] {
        assert_eq!(next["messages"][i], first["messages"][i]);
    }
    assert_eq!(next["messages"][2]["content"][0]["tool_use_id"], "t1");
    assert!(!requests[1].headers.contains_key("content-length"));
    assert_eq!(
        requests[0].headers["authorization"],
        requests[1].headers["authorization"]
    );
}

#[tokio::test]
async fn repeated_byte_limit_is_terminal_and_explains_the_actual_retry_without_private_body() {
    let client = Client::new(vec![
        Reply::status(413, json!({})),
        Reply::status(413, json!({"message":"private prompt private token"})),
    ]);
    let p = plugin(client.clone(), Arc::new(Store::default())).await;
    let response = p.fetch(with_attachments()).await.unwrap();
    assert_eq!(response.status, 413);
    let body: Value = serde_json::from_str(&fixture::body(response).await).unwrap();
    assert_eq!(body["error"]["type"], "request_too_large");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("2 older"));
    assert!(message.contains("byte"));
    assert!(!message.contains("private"));
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn byte_limit_without_older_attachments_or_on_count_tokens_does_not_replay() {
    let mut count_tokens = with_attachments();
    count_tokens.url.push_str("/count_tokens");
    for request in [anthro(), count_tokens] {
        let client = Client::new(vec![Reply::status(413, json!({}))]);
        let p = plugin(client.clone(), Arc::new(Store::default())).await;
        let response = p.fetch(request).await.unwrap();
        assert_eq!(response.status, 413);
        assert!(!fixture::body(response).await.contains("older"));
        assert_eq!(client.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn attachment_retry_returns_other_http_failure_without_another_size_retry() {
    for status in [401, 403, 429, 500] {
        let client = Client::new(vec![
            Reply::status(413, json!({})),
            Reply::status(
                status,
                json!({"message":"Requested active organization is not accessible by this user"}),
            ),
        ]);
        let p = plugin(client.clone(), Arc::new(Store::default())).await;
        assert_eq!(p.fetch(with_attachments()).await.unwrap().status, status);
        assert_eq!(client.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn rejected_attachment_body_is_dropped_without_waiting_before_retry() {
    struct Cancelled(Arc<AtomicBool>);
    impl Drop for Cancelled {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    struct UnreadableLimit {
        first: AtomicBool,
        cancelled: Arc<AtomicBool>,
        replies: Arc<Client>,
    }
    #[async_trait]
    impl nora_ai::HttpClient for UnreadableLimit {
        async fn stream(
            &self,
            request: HttpRequest,
        ) -> Result<nora_ai::HttpStreamingResponse, nora_ai::HttpClientError> {
            if self.first.swap(false, Ordering::SeqCst) {
                let cancelled = Cancelled(self.cancelled.clone());
                return Ok(nora_ai::HttpStreamingResponse {
                    status: 413,
                    headers: Default::default(),
                    body: Box::pin(futures::stream::once(async move {
                        let _cancelled = cancelled;
                        std::future::pending::<()>().await;
                        Ok(Vec::<u8>::new().into())
                    })),
                });
            }
            assert!(self.cancelled.load(Ordering::SeqCst));
            self.replies.stream(request).await
        }
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let replies = Client::new(vec![Reply::json(json!({"content":[]}))]);
    let client = Arc::new(UnreadableLimit {
        first: AtomicBool::new(true),
        cancelled: cancelled.clone(),
        replies: replies.clone(),
    });
    let p = Factory {
        client: Some(client),
        ..Default::default()
    };
    p.initialize(initialize()).await.unwrap();
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        p.fetch(with_attachments()),
    )
    .await
    .expect("A rejected body must not delay retry")
    .unwrap();
    assert_eq!(response.status, 200);
    assert!(cancelled.load(Ordering::SeqCst));
    assert_eq!(replies.requests.lock().unwrap().len(), 1);
}
