use super::*;
use base64::Engine;
use futures::{stream, StreamExt};
use nora_ai::{HttpClientError, HttpStreamingResponse};
use std::collections::VecDeque;
use std::sync::Mutex;

struct Reply {
    status: u16,
    body: Vec<u8>,
    headers: BTreeMap<String, String>,
}
impl Reply {
    fn json(value: Value) -> Self {
        Self {
            status: 200,
            body: serde_json::to_vec(&value).unwrap(),
            headers: BTreeMap::new(),
        }
    }
    fn raw(status: u16, body: &[u8]) -> Self {
        Self {
            status,
            body: body.to_vec(),
            headers: BTreeMap::new(),
        }
    }
}
struct MockHttp {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<Reply>>,
}
#[async_trait::async_trait]
impl HttpClient for MockHttp {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected Kimi Work HTTP request");
        Ok(HttpStreamingResponse {
            status: reply.status,
            headers: reply.headers,
            body: Box::pin(stream::iter(vec![Ok(reply.body.into())])),
        })
    }
}
fn provider(replies: Vec<Reply>) -> (KimiWork, Arc<MockHttp>) {
    let client = Arc::new(MockHttp {
        requests: Mutex::new(Vec::new()),
        replies: Mutex::new(replies.into()),
    });
    let manifest: Value = serde_json::from_str(include_str!("../nora-provider.json")).unwrap();
    let provider = KimiWork {
        config: RwLock::new(Some(Config {
            proxy: None,
            auth_methods: manifest["authMethods"].clone(),
            preset_models: manifest["models"].clone(),
        })),
        client: Some(client.clone()),
    };
    (provider, client)
}
fn model_payload() -> Value {
    serde_json::from_str(include_str!("fixtures/work-models.redacted.json")).unwrap()
}
fn credentials() -> Value {
    json!({"type":"oauth","access":"synthetic-work-token","membershipToken":"synthetic-membership-token"})
}
fn context() -> CallContext {
    CallContext {
        subscription_accounts_enabled: true,
        account: "work".into(),
        credentials: Some(credentials()),
        network_proxy: None,
        ..Default::default()
    }
}
fn request(url: &str, method: &str, body: &[u8]) -> FetchRequest {
    FetchRequest {
        caller_user_agent: None,
        constraints: Default::default(),
        subscription_accounts_enabled: true,
        account: "work".into(),
        credentials: Some(credentials()),
        network_proxy: None,
        metadata: BTreeMap::new(),
        body_file: None,
        url: url.into(),
        method: method.into(),
        body: Some(base64::engine::general_purpose::STANDARD.encode(body)),
        headers: BTreeMap::from([
            ("Authorization".into(), "Bearer wrong-code-token".into()),
            ("Cookie".into(), "wrong-cookie".into()),
            ("X-API-Key".into(), "wrong-api-key".into()),
            ("Origin".into(), "https://foreign.invalid".into()),
            ("X-Request-Tag".into(), "synthetic-tag".into()),
            ("User-Agent".into(), "caller-agent".into()),
            ("Content-Type".into(), "application/wrong".into()),
        ]),
    }
}

#[tokio::test]
async fn startup_never_discovers_or_imports_an_account() {
    let (provider, client) = provider(Vec::new());
    assert_eq!(
        provider.accounts(CallContext::default()).await.unwrap(),
        json!([])
    );
    assert_eq!(
        provider.models(CallContext::default()).await.unwrap()["source"],
        "preset"
    );
    assert!(provider
        .authorize(json!({"method":"desktop","inputs":{}}))
        .await
        .is_err());
    assert!(client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn pasted_tokens_use_separate_work_and_membership_services_and_expose_only_safe_profile_fields(
) {
    let (provider, client) = provider(vec![
        Reply::json(model_payload()),
        Reply::json(
            json!({"subscriptionBalance":{"amountUsedRatio":0.42},"ratelimitCode5h":{"ratio":1}}),
        ),
    ]);
    let result = provider.authorize(json!({"method":"tokens","inputs":{"workToken":"synthetic-work-token","membershipToken":"synthetic-membership-token"}})).await.unwrap();
    assert_eq!(result["credentials"]["access"], "synthetic-work-token");
    assert_eq!(
        result["credentials"]["membershipToken"],
        "synthetic-membership-token"
    );
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].url, MODELS);
    assert_eq!(
        requests[0].headers["authorization"],
        "Bearer synthetic-work-token"
    );
    assert_eq!(requests[1].url, MEMBERSHIP);
    assert_eq!(
        requests[1].headers["authorization"],
        "Bearer synthetic-membership-token"
    );
    assert_eq!(requests[1].headers["connect-protocol-version"], "1");
    assert_eq!(requests[1].body.as_deref(), Some(&b"{}"[..]));
    drop(requests);
    let profile = provider.accounts(context()).await.unwrap();
    assert_eq!(profile[0]["id"], "work");
    assert!(!profile.to_string().contains("synthetic-work-token"));
    assert!(!profile.to_string().contains("synthetic-membership-token"));
}

#[tokio::test]
async fn inference_preserves_messages_tools_and_streaming_while_replacing_all_caller_credentials() {
    let stream = b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\ndata: [DONE]\n\n";
    let (provider, client) = provider(vec![Reply::raw(200, stream)]);
    let body = br#"{"model":"k3-agent","messages":[{"role":"system","content":"private prompt"}],"tools":[{"type":"function","function":{"name":"Read"}}],"stream":true}"#;
    let mut response = provider.fetch(request(CHAT, "POST", body)).await.unwrap();
    let mut received = Vec::new();
    while let Some(chunk) = response.body.next().await {
        received.extend(chunk.unwrap());
    }
    assert_eq!(received, stream);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].url, CHAT);
    assert_eq!(requests[0].body.as_deref(), Some(&body[..]));
    assert_eq!(
        requests[0].headers["authorization"],
        "Bearer synthetic-work-token"
    );
    assert_eq!(requests[0].headers["X-Request-Tag"], "synthetic-tag");
    assert_eq!(
        requests[0]
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
            .count(),
        1
    );
    assert_eq!(
        requests[0]
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .count(),
        1
    );
    assert!(requests[0].headers.keys().all(|name| !matches!(
        name.to_ascii_lowercase().as_str(),
        "cookie" | "x-api-key" | "origin"
    )));
    assert!(!serde_json::to_string(&requests[0].headers)
        .unwrap()
        .contains("synthetic-membership-token"));
}

#[tokio::test]
async fn actual_inference_rewrites_official_moonshot_schemas_from_inline_and_verified_body_files() {
    let schema: Value =
        serde_json::from_str(include_str!("fixtures/zod-derived-refs.json")).unwrap();
    let body=serde_json::to_vec(&json!({"model":"k3-agent","messages":[{"role":"user","content":"hi"}],"tools":[{"type":"function","function":{"name":"tool","parameters":schema}}],"stream":true})).unwrap();
    for file in [false, true] {
        let (provider, client) = provider(vec![Reply::raw(200, b"data: [DONE]\n\n")]);
        let mut req = request(CHAT, "POST", &body);
        let path = std::env::temp_dir().join(format!(
            "nora-kimi-work-body-{}",
            nora_ai::oauth_pkce::random_hex(12)
        ));
        if file {
            use sha2::{Digest, Sha256};
            std::fs::write(&path, &body).unwrap();
            req.body = None;
            req.body_file=Some(serde_json::from_value(json!({"path":path,"length":body.len(),"sha256":format!("{:x}",Sha256::digest(&body))})).unwrap());
        }
        provider.fetch(req).await.unwrap();
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests[0].body, Some(moonshot::rewrite(body.clone())));
        if file {
            std::fs::remove_file(path).unwrap();
        }
    }
}

#[tokio::test]
async fn nonofficial_addresses_and_methods_are_refused_before_any_network_request() {
    let (provider, client) = provider(Vec::new());
    for (url, method) in [
        ("https://api.kimi.com/coding/v1/chat/completions", "POST"),
        ("https://foreign.invalid/chat/completions", "POST"),
        (
            "https://agent-gw.kimi.com/coding/v1/chat/completions?api_key=private",
            "POST",
        ),
        (CHAT, "GET"),
    ] {
        assert!(provider.fetch(request(url, method, b"{}")).await.is_err());
    }
    let mut manifest: Value = serde_json::from_str(include_str!("../nora-provider.json")).unwrap();
    manifest["baseURL"] = json!("https://foreign.invalid/coding/v1");
    let init = InitializeParams {
        subscription_accounts_enabled: true,
        installed_manifest: None,
        runtime_config: serde_json::Value::Null,
        api_version: API_VERSION,
        plugin_id: "kimi-work".into(),
        plugin_directory: "/synthetic/plugin".into(),
        data_directory: "/synthetic/data".into(),
        manifest,
    };
    assert!(provider.initialize(init).await.is_err());
    assert!(client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn upstream_errors_keep_rate_quota_and_retry_details_without_leaking_tokens() {
    for (status, message) in [
        (429, "Too many requests"),
        (429, "Plan quota exhausted"),
        (503, "Upstream unavailable"),
    ] {
        let body = json!({"error":{"message":message,"details":"synthetic-work-token synthetic-membership-token","resetsAt":"2030-01-01T00:00:00Z"}});
        let mut reply = Reply::raw(status, &serde_json::to_vec(&body).unwrap());
        reply.headers.insert("retry-after".into(), "30".into());
        let (provider, _) = provider(vec![reply]);
        let mut response = provider.fetch(request(CHAT, "POST", b"{}")).await.unwrap();
        assert_eq!(response.status, status);
        assert_eq!(response.headers["retry-after"], "30");
        let bytes = response.body.next().await.unwrap().unwrap();
        let parsed: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["error"]["message"], message);
        assert_eq!(parsed["error"]["resetsAt"], "2030-01-01T00:00:00Z");
        assert!(!parsed.to_string().contains("synthetic-work-token"));
        assert!(!parsed.to_string().contains("synthetic-membership-token"));
    }
}

#[tokio::test]
async fn expired_tokens_and_malformed_usage_do_not_expose_server_bodies() {
    for (status, body) in [
        (401, b"synthetic-work-token".as_slice()),
        (403, b"synthetic-membership-token".as_slice()),
        (200, b"malformed synthetic-membership-token".as_slice()),
    ] {
        let (provider, _) = provider(vec![Reply::raw(status, body)]);
        let error = provider.usage(context()).await.err().unwrap();
        assert!(!error.to_string().contains("synthetic-work-token"));
        assert!(!error.to_string().contains("synthetic-membership-token"));
        if status == 401 {
            assert_eq!(error.code, "authentication_required");
        }
        if status == 403 {
            assert_eq!(error.code, "upstream_transport_error");
        }
    }
    let (provider, _) = provider(vec![Reply::raw(401, b"private sign-in")]);
    let error = provider
        .fetch(request(CHAT, "POST", b"{}"))
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, "authentication_required");
}

struct SelectedSource(std::path::PathBuf);
impl SelectedSource {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "nora-kimi-work-connection-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path.join("config.json"))
    }
    fn write(&self, user: &str, work_token: &str, membership_token: &str) {
        std::fs::write(
            &self.0,
            serde_json::to_vec(&json!({"credentials":{
                "kimiCode":{"baseUrl":BASE,"apiKey":work_token},
                "kimiWeb":{"userId":user,"accessToken":membership_token}
            }}))
            .unwrap(),
        )
        .unwrap();
    }
}
impl Drop for SelectedSource {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

#[tokio::test]
async fn selecting_a_desktop_source_saves_only_a_reference_and_reads_each_live_token_without_refreshing_it(
) {
    let selected = SelectedSource::new();
    selected.write("fixture-user", "initial-work", "initial-membership");
    let original = std::fs::read(&selected.0).unwrap();
    let (provider, client) = provider(vec![
        Reply::json(model_payload()),
        Reply::raw(200, b"data: [DONE]\n\n"),
        Reply::json(json!({"subscriptionBalance":{"amountUsedRatio":0.25}})),
    ]);
    let authorized = provider
        .authorize(json!({"method":"desktop","inputs":{"configPath":selected.0}}))
        .await
        .unwrap();
    let account = authorized["credentials"].clone();
    assert!(!account.to_string().contains("initial-work"));
    assert!(!account.to_string().contains("initial-membership"));
    assert_eq!(std::fs::read(&selected.0).unwrap(), original);
    selected.write("fixture-user", "rotated-work", "rotated-membership");
    let updated = std::fs::read(&selected.0).unwrap();
    provider
        .refresh(json!({"credentials":account,"force":true}))
        .await
        .unwrap();
    assert_eq!(std::fs::read(&selected.0).unwrap(), updated);
    let mut chat = request(CHAT, "POST", b"{}");
    chat.credentials = Some(account.clone());
    provider.fetch(chat).await.unwrap();
    let usage = provider
        .usage(CallContext {
            subscription_accounts_enabled: true,
            account: "desktop".into(),
            credentials: Some(account.clone()),
            network_proxy: None,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(usage["items"][0]["usedPercent"], 25.0);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].headers["authorization"], "Bearer initial-work");
    assert_eq!(requests[1].headers["authorization"], "Bearer rotated-work");
    assert_eq!(
        requests[2].headers["authorization"],
        "Bearer rotated-membership"
    );
    drop(requests);
    selected.write("different-user", "different-work", "different-membership");
    assert!(provider
        .models(CallContext {
            subscription_accounts_enabled: true,
            account: "desktop".into(),
            credentials: Some(account),
            network_proxy: None,
            ..Default::default()
        })
        .await
        .is_err());
    assert_eq!(client.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn models_and_usage_read_only_their_actual_work_services() {
    let (provider, client) = provider(vec![
        Reply::json(model_payload()),
        Reply::json(json!({
            "subscriptionBalance":{"amountUsedRatio":0.42,"expireTime":"2030-01-01T00:00:00Z"},
            "ratelimitCode5h":{"ratio":1}, "ratelimitCode7d":{"ratio":1}
        })),
    ]);
    let catalog = provider.models(context()).await.unwrap();
    assert_eq!(catalog["source"], "remote");
    assert_eq!(catalog["models"][0]["id"], "k3-agent");
    let usage = provider.usage(context()).await.unwrap();
    assert_eq!(usage["items"].as_array().unwrap().len(), 1);
    assert_eq!(usage["items"][0]["usedPercent"], 42.0);
    assert!(usage["items"][0].get("resetsAt").is_none());
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].url, MODELS);
    assert_eq!(requests[1].url, MEMBERSHIP);
    assert!(requests
        .iter()
        .all(|request| !request.url.contains("api.kimi.com")));
}

#[tokio::test]
async fn oversized_metadata_responses_are_rejected_without_persisting_or_returning_raw_contents() {
    let body = vec![b'x'; 2 * 1024 * 1024 + 1];
    let (provider, _) = provider(vec![Reply::raw(200, &body)]);
    let error = provider.models(context()).await.err().unwrap();
    assert_eq!(error.code, "upstream_transport_error");
    assert!(error.message.contains("too large"));
    assert!(error.message.len() < 200);
}
