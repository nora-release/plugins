use super::*;
use crate::{
    auth::tests::saved,
    test_support::{Client, Directory, Reply},
};
use base64::Engine;
use futures::StreamExt;

async fn configured(client: Arc<dyn HttpClient>) -> (Directory, NousPortal) {
    let directory = Directory::new();
    let mut provider = NousPortal::default();
    provider.client = Some(client);
    provider
        .initialize(InitializeParams {
            api_version: 2,
            subscription_accounts_enabled: true,
            plugin_id: "nous-portal".into(),
            plugin_directory: "/test/package".into(),
            data_directory: directory.0.clone(),
            manifest: serde_json::from_str(include_str!("../nora-plugin.json")).unwrap(),
            installed_manifest: None,
            runtime_config: Value::Null,
        })
        .await
        .unwrap();
    (directory, provider)
}
fn context() -> CallContext {
    CallContext {
        credentials: Some(saved(3600_000)),
        account: "saved-account".into(),
        ..CallContext::default()
    }
}
fn request(headers: BTreeMap<String, String>, body: &[u8]) -> FetchRequest {
    serde_json::from_value(json!({"account":"saved-account","credentials":saved(3600_000),"url":format!("{}/chat/completions?trace=one",auth::INFERENCE),"method":"POST","headers":headers,"body":base64::engine::general_purpose::STANDARD.encode(body)})).unwrap()
}
#[tokio::test]
async fn initialization_does_not_read_or_change_hermes_or_start_a_sign_in() {
    let client = Client::new(vec![]);
    let (directory, provider) = configured(client.clone()).await;
    assert_eq!(
        provider.accounts(CallContext::default()).await.unwrap(),
        json!([])
    );
    assert_eq!(
        provider.models(CallContext::default()).await.unwrap()["models"][0]["id"],
        "Hermes-4-70B"
    );
    assert!(client.requests.lock().unwrap().is_empty());
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 0);
}
#[tokio::test]
async fn models_preserve_slash_ids_and_empty_live_catalog_is_authoritative() {
    let client = Client::new(vec![
        Reply::json(
            200,
            json!({"data":[{"id":"openai/gpt-6-luna"},{"id":"Nous/Hermes"}]}),
        ),
        Reply::json(200, json!({"data":[]})),
    ]);
    let (_directory, provider) = configured(client.clone()).await;
    let models = provider.models(context()).await.unwrap();
    assert_eq!(models["models"][0]["id"], "openai/gpt-6-luna");
    assert_eq!(models["models"][0]["contextWindow"], 0);
    assert_eq!(models["models"][0]["reasoning"], false);
    let empty = provider.models(context()).await.unwrap();
    assert_eq!(empty["models"], json!([]));
    assert_eq!(empty["authoritativeEmpty"], true);
    assert_eq!(
        client.requests.lock().unwrap()[0].url,
        format!("{}/models", auth::INFERENCE)
    );
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn failed_or_malformed_catalogs_never_replace_entitlements_with_an_empty_success() {
    for reply in [
        Reply::raw(401, "private response"),
        Reply::raw(503, "unavailable"),
        Reply::json(200, json!({"models":[]})),
        Reply::json(200, json!({"data":[{}]})),
        Reply::raw(200, "not json"),
    ] {
        let client = Client::new(vec![reply]);
        let (_directory, provider) = configured(client).await;
        assert!(provider.models(context()).await.is_err());
    }
}
#[tokio::test]
async fn inference_keeps_exact_request_and_sse_bytes_and_strips_other_credentials() {
    let sse = "data: {\"tool_calls\":[{\"id\":\"call-1\"}]}\n\ndata: [DONE]\n\n";
    let mut reply = Reply::raw(200, sse);
    reply.headers = BTreeMap::from([
        ("content-encoding".into(), "gzip".into()),
        ("content-length".into(), "1000".into()),
        ("content-type".into(), "text/event-stream".into()),
    ]);
    let client = Client::new(vec![reply]);
    let (_directory, provider) = configured(client.clone()).await;
    let body = b"{\"model\":\"openai/gpt-6-luna\", \"messages\":[],\"stream\":true}";
    let headers = [
        "authorization",
        "x-api-key",
        "x-nous-refresh-token",
        "cookie",
        "proxy-authorization",
        "host",
        "content-length",
        "x-magpie-session",
        "x-nora-session",
    ]
    .into_iter()
    .map(|k| (k.into(), "other-private".into()))
    .chain([("x-client-request-id".into(), "request-one".into())])
    .collect();
    let response = provider.fetch(request(headers, body)).await.unwrap();
    assert!(!response.headers.contains_key("content-encoding"));
    assert!(!response.headers.contains_key("content-length"));
    let bytes = response
        .body
        .map(|chunk| chunk.unwrap())
        .collect::<Vec<_>>()
        .await
        .concat();
    assert_eq!(bytes, sse.as_bytes());
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].body.as_deref(), Some(body.as_slice()));
    assert_eq!(requests[0].headers["x-client-request-id"], "request-one");
    assert!(requests[0].headers["authorization"].starts_with("Bearer e30."));
    assert!(!format!("{:?}", requests[0]).contains("other-private"));
}
#[tokio::test]
async fn inference_refuses_foreign_origins_ports_and_paths_before_any_network_request() {
    for url in [
        "https://evil.test/v1/chat/completions",
        "https://inference-api.nousresearch.com:8443/v1/chat/completions",
        "https://user@inference-api.nousresearch.com/v1/models",
        "https://inference-api.nousresearch.com/v1/responses",
        "http://inference-api.nousresearch.com/v1/models",
    ] {
        let client = Client::new(vec![]);
        let (_directory, provider) = configured(client.clone()).await;
        let mut request = request(BTreeMap::new(), b"{}");
        request.url = url.into();
        request.credentials = Some(saved(-1));
        assert!(provider.fetch(request).await.is_err());
        assert!(client.requests.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn upstream_refusals_are_returned_without_an_extra_plugin_retry() {
    for status in [401, 403, 429, 500] {
        let client = Client::new(vec![Reply::raw(status, "{\"error\":\"upstream\"}")]);
        let (_directory, provider) = configured(client.clone()).await;
        let response = provider
            .fetch(request(BTreeMap::new(), b"{}"))
            .await
            .unwrap();
        assert_eq!(response.status, status);
        assert_eq!(client.requests.lock().unwrap().len(), 1);
    }
}
#[tokio::test]
async fn models_use_a_snapshot_without_redeeming_or_saving_any_refresh_token() {
    let client = Client::new(vec![Reply::json(200, json!({"data":[{"id":"one"}]}))]);
    let (_directory, provider) = configured(client.clone()).await;
    provider.models(context()).await.unwrap();
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    assert!(provider.saved.lock().await.is_empty());
}
#[tokio::test]
async fn per_account_proxy_is_checked_and_welcome_inference_stays_on_the_issued_base() {
    let client = Client::new(vec![]);
    let (_directory, provider) = configured(client.clone()).await;
    let mut invalid_context = context();
    invalid_context.network_proxy = Some(NetworkProxy::Proxy {
        url: "file:///private".into(),
    });
    assert!(provider.models(invalid_context).await.is_err());
    assert!(client.requests.lock().unwrap().is_empty());
    let client = Client::new(vec![Reply::json(
        200,
        json!({"data":[{"id":"welcome-model"}]}),
    )]);
    let (_directory, provider) = configured(client.clone()).await;
    let mut context = context();
    let mut meta = auth::metadata(context.credentials.as_ref().unwrap()).unwrap();
    meta.base_url = auth::WELCOME.into();
    context.credentials.as_mut().unwrap()["refresh"] = json!(serde_json::to_string(&meta).unwrap());
    provider.models(context).await.unwrap();
    assert_eq!(
        client.requests.lock().unwrap()[0].url,
        format!("{}/models", auth::WELCOME)
    );
}
#[tokio::test]
async fn account_identity_is_the_granted_subject_and_quota_is_not_invented() {
    let client = Client::new(vec![]);
    let (_directory, provider) = configured(client).await;
    let accounts = provider.accounts(context()).await.unwrap();
    assert_eq!(accounts[0]["accountId"], "member");
    assert_eq!(accounts[0]["id"], "saved-account");
    let usage = provider.usage(context()).await.unwrap();
    assert_eq!(usage["available"], false);
    assert!(usage.get("items").is_none());
}
