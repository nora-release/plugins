use super::*;
use base64::Engine;
use std::path::PathBuf;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
pub(super) struct Endpoint {
    pub(super) base: String,
    pub(super) requests: Arc<std::sync::Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Endpoint {
    pub(super) async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let captured = captured.clone();
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let mut buffer = [0; 4096];
                    let mut content_length = None;
                    loop {
                        let Ok(length) = socket.read(&mut buffer).await else {
                            return;
                        };
                        if length == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&buffer[..length]);
                        if let Some(offset) =
                            bytes.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                        {
                            let length = *content_length.get_or_insert_with(|| {
                                String::from_utf8_lossy(&bytes[..offset])
                                    .lines()
                                    .find_map(|line| {
                                        line.split_once(':')
                                            .filter(|(name, _)| {
                                                name.eq_ignore_ascii_case("content-length")
                                            })
                                            .and_then(|(_, value)| {
                                                value.trim().parse::<usize>().ok()
                                            })
                                    })
                                    .unwrap_or_default()
                            });
                            if bytes.len() >= offset + 4 + length {
                                break;
                            }
                        }
                        if bytes.len() > 65536 {
                            return;
                        }
                    }
                    captured
                        .lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&bytes).into_owned());
                    let body = r#"{"data":[{"id":"live-model"}],"balance":2}"#;
                    let response=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        Self {
            base,
            requests,
            task,
        }
    }
}
#[derive(Debug)]
struct Mixed;
#[async_trait]
impl ProviderAdapter for Mixed {
    async fn usage(
        &self,
        _auth: &nora_ai::ResolvedProviderAuth,
        _account: Option<&str>,
        _client: &dyn HttpClient,
    ) -> Result<Value, String> {
        Ok(
            json!({"available":true,"items":[{"label":"native-subscription","usedPercent":20}],"account":{},"balances":[]}),
        )
    }
}
#[tokio::test]
async fn installed_vendor_configuration_is_restored_for_signed_in_accounts_only() {
    let native = Endpoint::start().await;
    let custom = Endpoint::start().await;
    let original = json!({"id":"fixture","name":"Fixture","api":"openai-completions","baseURL":format!("{}/v1",native.base),"config":{"keyProvider":"fixture-key","headers":{"X-Native-Identity":"keep-native"}}});
    let mut original = original;
    original["config"]["options"] =
        json!([{"id":"testFeature","type":"boolean","label":"Test feature","default":false}]);
    original["config"]["testFeature"] = json!(false);
    let mut configured = original.clone();
    configured["config"]["testFeature"] = json!(true);
    configured["baseURL"] = json!(format!("{}/v1", custom.base));
    configured["api"] = json!("anthropic-messages");
    configured["config"]["headers"] = json!({"X-Private-Workspace":"custom-private"});
    configured["config"]["modelsURL"] =
        json!(format!("{}/live?token=model-query-private", custom.base));
    configured["config"]["balanceURL"] = json!(format!(
        "{}/balance?token=balance-query-private",
        custom.base
    ));
    configured["config"]["balancePath"] = json!("$balance");
    configured["config"]["balanceToken"] = json!("balance-private");
    configured["config"]["apiKeyAuth"] = serde_json::to_value(nora_ai::AuthScheme::ApiKeyHeader {
        name: "X-API-Private".into(),
    })
    .unwrap();
    configured["config"]["extraBody"] = json!({"!tier":"custom-tier"});
    let plugin = BundledProvider::new(Mixed);
    plugin
        .initialize(InitializeParams {
            subscription_accounts_enabled: true,
            api_version: API_VERSION,
            plugin_id: "fixture".into(),
            plugin_directory: PathBuf::new(),
            data_directory: std::env::temp_dir().join("nora-mixed-account-runtime"),
            manifest: crate::fixtures::package(configured),
            installed_manifest: Some(crate::fixtures::package(original)),
            runtime_config: serde_json::Value::Null,
        })
        .await
        .unwrap();
    let api = json!({"type":"apiKey","access":"api-only-private"});
    let signed_in = json!({"type":"oauth","access":"subscription-only-private"});
    for (credentials, base, api_name) in [
        (&api, custom.base.as_str(), "anthropic-messages"),
        (&signed_in, native.base.as_str(), "openai-completions"),
    ] {
        let definition = plugin
            .configuration()
            .unwrap()
            .account_definition(Some(credentials))
            .unwrap();
        assert_eq!(definition.base_url, format!("{base}/v1"));
        assert_eq!(definition.api, api_name);
        assert_eq!(
            definition.config.extra.get("testFeature"),
            Some(&json!(true))
        );
        let result = plugin
            .models(CallContext {
                subscription_accounts_enabled: true,
                account: "fixture".into(),
                credentials: Some(credentials.clone()),
                network_proxy: None,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(result["source"], "remote", "{result}");
        let request = FetchRequest {
            caller_user_agent: None,
            constraints: Default::default(),
            subscription_accounts_enabled: true,
            account: "fixture".into(),
            credentials: Some(credentials.clone()),
            network_proxy: None,
            url: format!(
                "{base}/v1/{}",
                if credentials == &api {
                    "messages"
                } else {
                    "chat/completions"
                }
            ),
            method: "POST".into(),
            headers: BTreeMap::new(),
            metadata: BTreeMap::new(),
            body: Some(
                base64::engine::general_purpose::STANDARD
                    .encode(br#"{"model":"live-model","messages":[]}"#),
            ),
            body_file: None,
        };
        plugin.fetch(request).await.unwrap();
    }
    let usage = plugin
        .usage(CallContext {
            subscription_accounts_enabled: true,
            account: "signed".into(),
            credentials: Some(signed_in),
            network_proxy: None,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(usage["items"][0]["label"], "native-subscription");
    let native_requests = native.requests.lock().unwrap();
    assert_eq!(native_requests.len(), 2);
    for request in native_requests.iter() {
        assert!(request.contains("subscription-only-private"));
        assert!(request
            .to_ascii_lowercase()
            .contains("x-native-identity: keep-native"));
        assert!(!request.contains("custom-private"));
        assert!(!request.contains("custom-tier"));
    }
    let custom_requests = custom.requests.lock().unwrap();
    assert_eq!(custom_requests.len(), 2);
    for request in custom_requests.iter() {
        assert!(request.contains("api-only-private"));
        assert!(!request.contains("subscription-only-private"));
        assert!(request
            .to_ascii_lowercase()
            .contains("x-private-workspace: custom-private"));
    }
    assert!(custom_requests[1].contains("custom-tier"));
}
