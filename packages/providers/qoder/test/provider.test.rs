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
            .expect("Unexpected Qoder network request");
        let bytes = value
            .get("raw")
            .and_then(Value::as_str)
            .map(|raw| raw.as_bytes().to_vec())
            .unwrap_or_else(|| value.to_string().into_bytes());
        let chunks = if value.get("split").and_then(Value::as_bool) == Some(true) {
            bytes.chunks(3).map(<[u8]>::to_vec).collect::<Vec<_>>()
        } else {
            vec![bytes]
        };
        let body: HttpByteStream = Box::pin(futures::stream::iter(
            chunks.into_iter().map(|bytes| Ok(bytes.into())),
        ));
        Ok(HttpStreamingResponse {
            status,
            headers: BTreeMap::from([(
                "content-type".into(),
                if value.get("raw").is_some() {
                    "text/event-stream".into()
                } else {
                    "application/json".into()
                },
            )]),
            body,
        })
    }
}
pub(crate) fn configured(site: Site, client: Arc<Client>) -> Qoder {
    let mut provider = Qoder::new(site);
    provider.client = Some(client);
    *provider.config.write().unwrap() = Some(Config {
        proxy: None,
        methods: json!([{"id":"oauth","type":"oauth"},{"id":"pat","type":"oauth"}]),
    });
    provider
}
pub(crate) fn credentials() -> Value {
    json!({"type":"oauth","access":"job-one","refresh":"refresh-one","expires":now()+86_400_000,"uid":"u1","email":"one@example.com","name":"One","machineId":"12345678-1234-1234-1234-123456789abc","deviceToken":"device-one","deviceRefresh":"device-refresh","accountId":"one@example.com"})
}
pub(crate) fn catalog() -> Value {
    json!({"app":[{"key":"model","display_name":"Account model","enable":true,"source":"source","max_input_tokens":200000,"is_reasoning":false,"price_factor":0.5}]})
}
pub(crate) fn env(chunk: Value) -> String {
    format!(
        "data: {}\n\n",
        json!({"statusCodeValue":200,"body":chunk.as_str().map(str::to_owned).unwrap_or_else(||chunk.to_string())})
    )
}
pub(crate) fn answer() -> Value {
    json!({"raw":format!("{}{}",env(json!({"choices":[{"delta":{"content":"hello"},"finish_reason":"stop"}]})),env(json!("[DONE]")))})
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
async fn both_sites_start_without_importing_or_authenticating_any_account() {
    for site in [Site::International, Site::China] {
        let client = Client::new(vec![]);
        let mut provider = Qoder::new(site);
        provider.client = Some(client.clone());
        let manifest: Value = serde_json::from_str(if site == Site::China {
            include_str!("../../qoder-cn/nora-plugin.json")
        } else {
            include_str!("../nora-plugin.json")
        })
        .unwrap();
        assert_eq!(
            manifest["contributes"]["provider"]["config"]["authTimeoutMs"],
            1050000
        );
        provider
            .initialize(InitializeParams {
                subscription_accounts_enabled: true,
                installed_manifest: None,
                runtime_config: serde_json::Value::Null,
                api_version: 2,
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
        assert!(
            provider.models(CallContext::default()).await.unwrap()["models"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(client.requests.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn public_and_enterprise_accounts_keep_separate_live_catalogs() {
    let client = Client::new(vec![
        (200, catalog()),
        (
            200,
            json!({"app":[{"key":"enterprise-only","enable":true}]}),
        ),
    ]);
    let provider = configured(Site::China, client.clone());
    let public = credentials();
    let mut enterprise = public.clone();
    enterprise["vpc"] = json!("acme");
    let a = provider
        .listing(client.as_ref(), &public, false)
        .await
        .unwrap();
    let b = provider
        .listing(client.as_ref(), &enterprise, false)
        .await
        .unwrap();
    assert_eq!(a[0].key, "model");
    assert_eq!(b[0].key, "enterprise-only");
    let requests = client.requests.lock().unwrap();
    assert!(requests[0].url.starts_with("https://gateway.qoder.com.cn/"));
    assert!(requests[1]
        .url
        .starts_with("https://acme-gateway.vpc.qoder.com.cn/"));
}
