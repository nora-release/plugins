use super::*;
use nora_ai::{HttpByteStream, HttpClientError};
use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;
pub(crate) struct Client {
    pub requests: StdMutex<Vec<HttpRequest>>,
    replies: StdMutex<VecDeque<(u16, Value)>>,
}
impl Client {
    pub fn new(replies: Vec<(u16, Value)>) -> Arc<Self> {
        Arc::new(Self {
            requests: StdMutex::new(Vec::new()),
            replies: StdMutex::new(replies.into()),
        })
    }
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        let (status, value) = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected Trae request");
        let (raw, kind) = if let Some(raw) = value.get("raw").and_then(Value::as_str) {
            (raw.as_bytes().to_vec(), "text/event-stream")
        } else {
            (value.to_string().into_bytes(), "application/json")
        };
        let chunks = if value.get("split").and_then(Value::as_bool) == Some(true) {
            raw.chunks(3).map(<[u8]>::to_vec).collect::<Vec<_>>()
        } else {
            vec![raw]
        };
        let body: HttpByteStream = Box::pin(futures::stream::iter(
            chunks.into_iter().map(|bytes| Ok(bytes.into())),
        ));
        Ok(HttpStreamingResponse {
            status,
            headers: BTreeMap::from([("content-type".into(), kind.into())]),
            body,
        })
    }
}
pub(crate) fn configured(site: Site, client: Arc<Client>) -> Trae {
    let mut provider = Trae::new(site);
    provider.client = Some(client);
    *provider.config.write().unwrap() = Some(Config {
        proxy: None,
        methods: json!([{"id":"oauth","type":"oauth"}]),
    });
    provider
}
pub(crate) fn credentials() -> Value {
    json!({"type":"oauth","access":"jwt-one","refresh":"refresh-one","expires":now()+86_400_000,"uid":"member","accountId":"member@example.com","deviceId":"1234567890123456","machineId":"machine-one","clientId":site::CLIENT})
}
pub(crate) fn listing() -> Value {
    json!({"function_configs":[{"function":"chat_v3","config_info_list":[{"config_name":"model","usage":"chat_completion","display_name":"Actual model","context_window_tokens":{"dev":200000},"model_detail_list":[{"model_name":"model__dev","max_tokens":65536}],"multimodal":true}]}]})
}
pub(crate) fn answer() -> Value {
    json!({"raw":"event: output\ndata: {\"response\":\"hello\"}\n\nevent: done\ndata: {}\n\n"})
}
pub(crate) async fn collect(mut response: FetchResponse) -> Vec<u8> {
    use futures::StreamExt;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.body.next().await {
        bytes.extend(chunk.unwrap());
    }
    bytes
}
#[tokio::test]
async fn both_sites_initialize_without_scanning_or_importing_desktop_accounts() {
    for site in [Site::China, Site::Global] {
        let client = Client::new(vec![]);
        let mut provider = Trae::new(site);
        provider.client = Some(client.clone());
        let manifest: Value = serde_json::from_str(if site == Site::China {
            include_str!("../nora-provider.json")
        } else {
            include_str!("../../trae-global/nora-provider.json")
        })
        .unwrap();
        assert_eq!(manifest["config"]["authTimeoutMs"], 600000);
        provider
            .initialize(InitializeParams {
                subscription_accounts_enabled: true,
                installed_manifest: None,
                runtime_config: serde_json::Value::Null,
                api_version: 1,
                plugin_id: site.id().into(),
                plugin_directory: "/test/plugin".into(),
                data_directory: "/test/data".into(),
                manifest,
            })
            .await
            .unwrap();
        assert_eq!(
            provider.accounts(CallContext::default()).await.unwrap(),
            json!([])
        );
        let empty = provider.models(CallContext::default()).await.unwrap();
        assert!(empty["models"].as_array().unwrap().is_empty());
        assert_eq!(empty["source"], "preset");
        assert_eq!(empty["allowUnknownModels"], false);
        assert!(client.requests.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn catalog_state_is_isolated_by_account_and_enterprise_deployment() {
    let provider = configured(Site::Global, Client::new(vec![]));
    let credentials = credentials();
    let a = provider.account(&credentials).await.unwrap();
    let mut other = credentials.clone();
    other["uid"] = json!("other");
    let b = provider.account(&other).await.unwrap();
    assert!(!Arc::ptr_eq(&a, &b));
    other["uid"] = credentials["uid"].clone();
    other["api"] = json!("https://enterprise.trae.ai");
    let c = provider.account(&other).await.unwrap();
    assert!(!Arc::ptr_eq(&a, &c));
}
