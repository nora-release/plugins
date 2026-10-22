use super::*;
use base64::Engine;
use std::sync::atomic::{AtomicBool, Ordering};
#[path = "fixture.rs"]
mod fixture;
#[derive(Default)]
struct Saver {
    fail: AtomicBool,
    values: std::sync::Mutex<Vec<Value>>,
}
#[async_trait]
impl Persistence for Saver {
    async fn save(&self, value: Value) -> PluginResult<()> {
        self.values.lock().unwrap().push(value);
        if self.fail.swap(false, Ordering::SeqCst) {
            Err(PluginError::new(
                "host_save_failed",
                "The host could not save credentials.",
            ))
        } else {
            Ok(())
        }
    }
}
async fn provider(replies: Vec<fixture::Reply>) -> (Kiro, Arc<fixture::Client>, Arc<Saver>) {
    let client = Arc::new(fixture::Client::new(replies));
    let saver = Arc::new(Saver::default());
    let mut provider = Kiro::default();
    provider.client = Some(client.clone());
    provider.persistence = saver.clone();
    provider
        .initialize(InitializeParams {
            api_version: API_VERSION,
            plugin_id: "kiro".into(),
            plugin_directory: Default::default(),
            data_directory: Default::default(),
            manifest: serde_json::from_str(include_str!("../nora-provider.json")).unwrap(),
        })
        .await
        .unwrap();
    (provider, client, saver)
}
fn context(credentials: Value) -> CallContext {
    CallContext {
        account: "work".into(),
        credentials: Some(credentials),
        network_proxy: None,
    }
}
fn fetch(credentials: Value) -> FetchRequest {
    FetchRequest{account:"work".into(),credentials:Some(credentials),network_proxy:None,url:format!("{}/v1/messages",session::BASE),method:"POST".into(),headers:BTreeMap::from([("authorization".into(),"caller-secret".into())]),metadata:BTreeMap::from([("reasoningEffort".into(),"high".into()),("sessionID".into(),"private-session".into())]),body:Some(base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&json!({"model":"auto","stream":true,"messages":[{"role":"user","content":"hello"}]})).unwrap())),body_file:None}
}
#[tokio::test]
async fn startup_and_disconnected_catalog_do_not_read_or_import_accounts() {
    let (provider, client, _) = provider(vec![]).await;
    assert!(provider
        .accounts(CallContext::default())
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        provider.models(CallContext::default()).await.unwrap()["source"],
        "preset"
    );
    assert!(client.requests.lock().unwrap().is_empty());
    let methods = provider.auth_methods(CallContext::default()).await.unwrap();
    assert_eq!(methods.as_array().unwrap().len(), 4);
}
#[tokio::test]
async fn models_usage_and_chat_use_the_account_region_and_do_not_forward_private_metadata() {
    let(provider,client,_)=provider(vec![fixture::Reply::json(json!({"models":[{"modelId":"auto","tokenLimits":{"maxInputTokens":200000,"maxOutputTokens":32000},"supportedInputTypes":["IMAGE"]}]})),fixture::Reply::json(fixture::usage()),fixture::Reply::raw(200,fixture::event("assistantResponseEvent",json!({"content":"hello"})))]).await;
    let credentials = fixture::saved();
    let models = provider.models(context(credentials.clone())).await.unwrap();
    assert_eq!(models["models"][0]["contextWindow"], 200000);
    let usage = provider.usage(context(credentials.clone())).await.unwrap();
    assert_eq!(usage["account"]["id"], "work");
    let response = provider.fetch(fetch(credentials)).await.unwrap();
    assert_eq!(response.status, 200);
    assert!(fixture::body(response).await.contains("hello"));
    let requests = client.requests.lock().unwrap();
    assert!(requests
        .iter()
        .all(|request| request.url.contains(".eu-central-1.kiro.dev/")));
    let last = requests.last().unwrap();
    assert_eq!(
        last.url,
        "https://runtime.eu-central-1.kiro.dev/generateAssistantResponse"
    );
    assert_eq!(last.headers["authorization"], "Bearer private-access");
    assert_eq!(last.headers["x-amzn-codewhisperer-optout"], "true");
    assert_eq!(last.headers["amz-sdk-request"], "attempt=1; max=1");
    let body: Value = serde_json::from_slice(last.body.as_ref().unwrap()).unwrap();
    assert!(body
        .pointer("/conversationState/currentMessage/userInputMessage/content")
        .unwrap()
        .as_str()
        .unwrap()
        .contains("<max_thinking_length>30000"));
    assert!(!body.to_string().contains("private-session"));
    assert!(!last
        .headers
        .values()
        .any(|value| value.contains("caller-secret")));
}
#[tokio::test]
async fn rotations_wait_for_host_ack_and_retry_the_pending_save_without_reusing_refresh() {
    let (provider, client, saver) = provider(vec![
        fixture::Reply::json(
            json!({"accessToken":"next-access","refreshToken":"next-refresh","expiresIn":3600}),
        ),
        fixture::Reply::json(fixture::usage()),
    ])
    .await;
    let mut credentials = fixture::saved();
    credentials["expires"] = json!(session::now() - 1);
    saver.fail.store(true, Ordering::SeqCst);
    assert_eq!(
        provider
            .usage(context(credentials.clone()))
            .await
            .unwrap_err()
            .code,
        "host_save_failed"
    );
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    let usage = provider.usage(context(credentials)).await.unwrap();
    assert_eq!(usage["available"], true);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].url.ends_with("/refreshToken"));
    assert_eq!(requests[1].headers["authorization"], "Bearer next-access");
    let saved = saver.values.lock().unwrap();
    assert_eq!(saved.len(), 2);
    assert_eq!(saved[0]["refresh"], "next-refresh");
    assert_eq!(saved[1]["access"], "next-access");
}
#[tokio::test]
async fn a_403_refreshes_an_owned_account_once_after_ack_before_retrying_chat() {
    let (provider, client, saver) = provider(vec![
        fixture::Reply::raw(403, b"private-access denied".to_vec()),
        fixture::Reply::json(
            json!({"accessToken":"next-access","refreshToken":"next-refresh","expiresIn":3600}),
        ),
        fixture::Reply::raw(
            200,
            fixture::event("assistantResponseEvent", json!({"content":"answer"})),
        ),
    ])
    .await;
    let response = provider.fetch(fetch(fixture::saved())).await.unwrap();
    assert_eq!(response.status, 200);
    assert!(fixture::body(response).await.contains("answer"));
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[0].headers["authorization"],
        "Bearer private-access"
    );
    assert!(requests[1]
        .url
        .contains("prod.eu-west-1.auth.desktop.kiro.dev/refreshToken"));
    assert_eq!(requests[2].headers["authorization"], "Bearer next-access");
    assert_eq!(saver.values.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn api_key_refusals_do_not_attempt_refresh_or_store_credentials() {
    let (provider, client, saver) = provider(vec![fixture::Reply::raw(
        403,
        b"private-key denied".to_vec(),
    )])
    .await;
    let mut credentials = fixture::saved();
    credentials["method"] = json!("apikey");
    let response = provider.fetch(fetch(credentials)).await.unwrap();
    assert_eq!(response.status, 403);
    assert!(!fixture::body(response).await.contains("private-key"));
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    assert!(saver.values.lock().unwrap().is_empty());
}
#[tokio::test]
async fn selected_source_is_read_only_bound_to_identity_and_never_copies_tokens() {
    let directory = std::env::temp_dir().join(format!(
        "nora-kiro-provider-{}",
        nora_ai::oauth_pkce::random_hex(12)
    ));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("kiro-auth-token.json");
    let original=json!({"accessToken":"selected-token","refreshToken":"selected-refresh","expiresAt":session::now()+3600000,"authMethod":"social","profileArn":fixture::PROFILE}).to_string();
    std::fs::write(&path, &original).unwrap();
    let mut changed = fixture::usage();
    changed["userInfo"]["email"] = json!("different@example.test");
    let (provider, client, saver) = provider(vec![
        fixture::Reply::json(fixture::usage()),
        fixture::Reply::json(changed),
    ])
    .await;
    let result = provider
        .authorize(
            json!({"authMethodID":"ideSource","inputs":{"sourcePath":path.to_str().unwrap()}}),
        )
        .await
        .unwrap();
    let credentials = result["credentials"].clone();
    assert!(!credentials.to_string().contains("selected-token"));
    assert!(!credentials.to_string().contains("selected-refresh"));
    assert_eq!(credentials["source"]["kind"], "ide");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    let mut next: Value = serde_json::from_str(&original).unwrap();
    next["accessToken"] = json!("another-token");
    std::fs::write(&path, next.to_string()).unwrap();
    let error = provider.usage(context(credentials)).await.unwrap_err();
    assert_eq!(error.code, "authentication_required");
    assert_eq!(client.requests.lock().unwrap().len(), 2);
    assert!(saver.values.lock().unwrap().is_empty());
    std::fs::remove_dir_all(directory).unwrap();
}
#[tokio::test]
async fn expired_read_only_sources_require_refresh_in_their_owner_without_rotation() {
    let path = std::env::temp_dir().join(format!(
        "nora-kiro-expired-{}",
        nora_ai::oauth_pkce::random_hex(12)
    ));
    std::fs::write(&path,json!({"accessToken":"expired-token","refreshToken":"source-refresh","expiresAt":session::now()-1}).to_string()).unwrap();
    let (provider, client, saver) = provider(vec![]).await;
    let result = provider
        .authorize(
            json!({"authMethodID":"ideSource","inputs":{"sourcePath":path.to_str().unwrap()}}),
        )
        .await;
    assert!(result.is_err());
    assert!(client.requests.lock().unwrap().is_empty());
    assert!(saver.values.lock().unwrap().is_empty());
    std::fs::remove_file(path).unwrap();
}
#[tokio::test]
async fn invalid_endpoints_proxy_and_query_fail_before_any_upstream_request() {
    let (provider, client, _) = provider(vec![]).await;
    for url in [
        "https://foreign.invalid/v1/messages",
        "https://user:secret@runtime.us-east-1.kiro.dev/v1/messages",
        "https://runtime.us-east-1.kiro.dev/v1/messages?endpoint=evil",
        "https://runtime.us-east-1.kiro.dev/other",
    ] {
        let mut request = fetch(fixture::saved());
        request.url = url.into();
        assert!(provider.fetch(request).await.is_err());
    }
    let mut request = fetch(fixture::saved());
    request.network_proxy = Some(NetworkProxy::Proxy {
        url: "ftp://user:private-password@proxy.invalid".into(),
    });
    let error = provider.fetch(request).await.err().unwrap();
    assert!(!error.message.contains("private-password"));
    assert!(client.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn api_key_authorization_uses_real_profile_and_account_queries_and_redacts_accounts() {
    let (provider, client, _) = provider(vec![
        fixture::Reply::json(json!({"profile":{"arn":fixture::PROFILE}})),
        fixture::Reply::json(fixture::usage()),
    ])
    .await;
    let result = provider
        .authorize(json!({"authMethodID":"apiKey","inputs":{"apiKey":"ksk_private"}}))
        .await
        .unwrap();
    let accounts = provider
        .accounts(context(result["credentials"].clone()))
        .await
        .unwrap();
    assert_eq!(accounts[0]["email"], "person@example.test");
    assert!(!accounts.to_string().contains("ksk_private"));
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].headers["tokentype"], "API_KEY");
    assert_eq!(requests[1].headers["tokentype"], "API_KEY");
}
#[tokio::test]
async fn explicit_refresh_uses_the_same_acknowledged_rotation_path() {
    let (provider, client, saver) = provider(vec![fixture::Reply::json(
        json!({"accessToken":"next","refreshToken":"rotated","expiresIn":3600}),
    )])
    .await;
    let result = provider
        .refresh(json!({"credentials":fixture::saved(),"force":true}))
        .await
        .unwrap();
    assert_eq!(result["changed"], false);
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    assert_eq!(saver.values.lock().unwrap()[0]["refresh"], "rotated");
}
