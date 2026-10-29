use super::*;
use nora_ai::{HttpMethod, HttpRequest};
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn endpoint() -> (String, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut bytes = Vec::new();
        let mut byte = [0u8; 1];
        while !bytes.ends_with(b"\r\n\r\n") {
            socket.read_exact(&mut byte).await.unwrap();
            bytes.push(byte[0]);
        }
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await
            .unwrap();
        String::from_utf8(bytes).unwrap()
    });
    (format!("http://{address}"), server)
}

async fn configured(adapter: impl ProviderAdapter, proxy: Option<NetworkProxy>) -> BundledProvider {
    let provider = BundledProvider::new(adapter);
    let mut manifest: Value =
        serde_json::from_str(include_str!("../../xai/nora-plugin.json")).unwrap();
    manifest["contributes"]["provider"]["config"]["networkProxy"] =
        serde_json::to_value(proxy).unwrap();
    provider
        .initialize(InitializeParams {
            subscription_accounts_enabled: true,
            installed_manifest: None,
            runtime_config: Value::Null,
            api_version: API_VERSION,
            plugin_id: "xai".into(),
            plugin_directory: PathBuf::new(),
            data_directory: PathBuf::new(),
            manifest,
        })
        .await
        .unwrap();
    provider
}

#[tokio::test]
async fn default_proxy_and_explicit_direct_reach_the_selected_transport() {
    let (proxy, proxy_request) = endpoint().await;
    let provider = configured(
        crate::GenericProvider,
        Some(NetworkProxy::Proxy { url: proxy }),
    )
    .await;
    let configuration = provider.configuration().unwrap();
    let response = configuration
        .params_client(&json!({}))
        .unwrap()
        .request(HttpRequest::new(
            HttpMethod::Get,
            "http://upstream.invalid/proxied",
        ))
        .await
        .unwrap();
    assert_eq!(response.body, b"ok");
    assert!(proxy_request
        .await
        .unwrap()
        .starts_with("GET http://upstream.invalid/proxied HTTP/1.1"));
    let (origin, direct_request) = endpoint().await;
    let response = configuration
        .params_client(&json!({"networkProxy":{"mode":"direct"}}))
        .unwrap()
        .request(HttpRequest::new(
            HttpMethod::Get,
            format!("{origin}/direct"),
        ))
        .await
        .unwrap();
    assert_eq!(response.body, b"ok");
    assert!(direct_request
        .await
        .unwrap()
        .starts_with("GET /direct HTTP/1.1"));
}

#[tokio::test]
async fn invalid_network_overrides_never_use_the_default_transport() {
    let provider = configured(crate::GenericProvider, None).await;
    let error = provider.configuration().unwrap().params_client(&json!({"networkProxy":{"mode":"proxy","url":"ftp://private-user:private-password@proxy.invalid"}}));
    assert!(error.is_err());
    let message = error.err().unwrap().to_string();
    assert!(!message.contains("private-user"));
    assert!(!message.contains("private-password"));
}

#[derive(Debug, Clone)]
struct SourceNetworkRecorder(Arc<std::sync::Mutex<Vec<Option<NetworkProxy>>>>);

#[async_trait]
impl ProviderAdapter for SourceNetworkRecorder {
    fn is_source(&self, _credentials: &OAuthCredentials) -> bool {
        true
    }
    async fn authorize_source(
        &self,
        _method: &str,
        _client: &dyn HttpClient,
        proxy: Option<&NetworkProxy>,
    ) -> PluginResult<OAuthCredentials> {
        self.0.lock().unwrap().push(proxy.cloned());
        Ok(OAuthCredentials::api_key(OAuthProviderId::XAI, ""))
    }
    async fn refresh_source(
        &self,
        _credentials: &OAuthCredentials,
        _force: bool,
        proxy: Option<&NetworkProxy>,
    ) -> PluginResult<()> {
        self.0.lock().unwrap().push(proxy.cloned());
        Ok(())
    }
    async fn resolve_source(
        &self,
        _credentials: &OAuthCredentials,
        proxy: Option<&NetworkProxy>,
    ) -> PluginResult<nora_ai::ResolvedProviderAuth> {
        self.0.lock().unwrap().push(proxy.cloned());
        Err(PluginError::authentication(
            "Fixture source stopped before HTTP.",
        ))
    }
}

#[tokio::test]
async fn every_source_operation_receives_the_effective_proxy_and_explicit_direct() {
    for override_proxy in [
        None,
        Some(NetworkProxy::Direct),
        Some(NetworkProxy::Proxy {
            url: "socks5h://account.invalid:1080".into(),
        }),
    ] {
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let default = NetworkProxy::Proxy {
            url: "http://provider.invalid:8080".into(),
        };
        let provider = configured(
            SourceNetworkRecorder(observed.clone()),
            Some(default.clone()),
        )
        .await;
        let effective = override_proxy.clone().unwrap_or(default);
        let authorized = provider
            .authorize(json!({"method":"grokCLI","networkProxy":override_proxy}))
            .await
            .unwrap();
        let credentials = authorized["credentials"].clone();
        provider
            .refresh(json!({"credentials":credentials,"force":true,"networkProxy":override_proxy}))
            .await
            .unwrap();
        let context = CallContext {
            subscription_accounts_enabled: true,
            account: "fixture".into(),
            credentials: Some(credentials.clone()),
            network_proxy: override_proxy.clone(),
            ..Default::default()
        };
        provider.models(context.clone()).await.unwrap();
        assert!(provider.usage(context).await.is_err());
        let request = FetchRequest {
            caller_user_agent: None,
            constraints: Default::default(),
            subscription_accounts_enabled: true,
            account: "fixture".into(),
            credentials: Some(credentials),
            network_proxy: override_proxy,
            method: "POST".into(),
            url: "https://cli-chat-proxy.grok.com/v1/responses".into(),
            headers: BTreeMap::new(),
            body: None,
            metadata: BTreeMap::new(),
            body_file: None,
        };
        assert!(provider.fetch(request).await.is_err());
        let observed = observed.lock().unwrap();
        assert_eq!(observed.len(), 5);
        assert!(observed
            .iter()
            .all(|proxy| proxy.as_ref() == Some(&effective)));
    }
}
