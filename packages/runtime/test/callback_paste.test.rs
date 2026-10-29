use super::*;
use nora_ai::{HttpClientError, HttpMethod, HttpRequest, HttpResponse, HttpStreamingResponse};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Default)]
struct CallbackClient {
    exchanges: AtomicUsize,
}
#[async_trait]
impl HttpClient for CallbackClient {
    async fn stream(&self, _: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        unreachable!()
    }
    async fn request(&self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        assert_eq!(request.method, HttpMethod::Post);
        assert_eq!(request.url, "https://auth.openai.com/oauth/token");
        self.exchanges.fetch_add(1, Ordering::SeqCst);
        Ok(HttpResponse{status:200,headers:BTreeMap::new(),body:serde_json::to_vec(&json!({"access_token":"fixture-access","refresh_token":"fixture-refresh","expires_in":3600})).unwrap()})
    }
}
#[derive(Debug)]
struct CallbackAdapter(Arc<CallbackClient>);
#[async_trait]
impl ProviderAdapter for CallbackAdapter {
    fn id(&self) -> Option<&'static str> {
        Some("openai")
    }
    async fn login(
        &self,
        callbacks: OAuthLoginCallbacks,
        _: &dyn HttpClient,
    ) -> Result<OAuthCredentials, OAuthError> {
        nora_provider_auth::OAuthLogin::login_openai_codex(callbacks, self.0.as_ref()).await
    }
}

#[tokio::test]
async fn callback_paste_runtime_codex_keeps_session_across_polls_and_bad_input() {
    let occupied = tokio::net::TcpListener::bind(("127.0.0.1", 1455))
        .await
        .unwrap();
    let client = Arc::new(CallbackClient::default());
    let provider = BundledProvider::new(CallbackAdapter(client.clone()));
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join("openai/nora-plugin.json"),
        )
        .unwrap(),
    )
    .unwrap();
    provider
        .initialize(InitializeParams {
            subscription_accounts_enabled: true,
            installed_manifest: None,
            runtime_config: Value::Null,
            api_version: API_VERSION,
            plugin_id: "openai".into(),
            plugin_directory: Default::default(),
            data_directory: Default::default(),
            manifest,
        })
        .await
        .unwrap();
    let start = provider
        .authorize(json!({"authMethodID":"oauth"}))
        .await
        .unwrap();
    assert_eq!(start["callbackMode"], "poll");
    assert_eq!(start["pasteCallback"], true);
    assert_eq!(start["callbackBlocked"], "busy");
    let fields: BTreeMap<_, _> = url::Url::parse(start["url"].as_str().unwrap())
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect();
    let session = start["session"].clone();
    let bad=provider.callback(json!({"session":session,"poll":true,"callbackURL":"https://evil.invalid/?code=private"})).await.unwrap();
    assert!(bad["inputError"].is_string());
    assert_eq!(client.exchanges.load(Ordering::SeqCst), 0);
    let good = format!(
        "{}?state={}&code=fixture",
        fields["redirect_uri"], fields["state"]
    );
    let mut result = provider
        .callback(json!({"session":session,"poll":true,"callbackURL":good}))
        .await
        .unwrap();
    for _ in 0..100 {
        if result.get("credentials").is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
        result = provider
            .callback(json!({"session":session,"poll":true}))
            .await
            .unwrap();
    }
    assert_eq!(result["credentials"]["access"], "fixture-access");
    assert_eq!(client.exchanges.load(Ordering::SeqCst), 1);
    assert!(provider
        .callback(json!({"session":session,"poll":true}))
        .await
        .is_err());
    assert!(provider.logins.lock().await.is_empty());
    assert_eq!(occupied.local_addr().unwrap().port(), 1455);
}
