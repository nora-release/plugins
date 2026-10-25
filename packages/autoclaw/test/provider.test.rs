use super::*;
use futures::StreamExt;
use nora_ai::{HttpByteStream, HttpClientError};
use std::{collections::VecDeque, sync::Mutex as StdMutex};

pub(crate) struct Client {
    pub requests: StdMutex<Vec<HttpRequest>>,
    pub replies: StdMutex<VecDeque<(u16, Value)>>,
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
            .expect("Unexpected AutoClaw upstream request");
        if value["offline"] == true {
            return Err(HttpClientError::Transport("simulated offline".into()));
        }
        let raw = value.get("raw").and_then(Value::as_str);
        let bytes = raw
            .map(|s| s.as_bytes().to_vec())
            .unwrap_or_else(|| value.to_string().into_bytes());
        let body: HttpByteStream = Box::pin(futures::stream::iter(
            bytes
                .chunks(3)
                .map(|b| Ok(b.to_vec().into()))
                .collect::<Vec<_>>(),
        ));
        Ok(HttpStreamingResponse {
            status,
            headers: BTreeMap::from([(
                "content-type".into(),
                if raw.is_some() {
                    "text/event-stream"
                } else {
                    "application/json"
                }
                .into(),
            )]),
            body,
        })
    }
}
#[derive(Default)]
pub(crate) struct Sink {
    pub writes: StdMutex<Vec<Value>>,
    pub reject: std::sync::atomic::AtomicBool,
}
#[async_trait]
impl CredentialSink for Sink {
    async fn save(&self, value: Value) -> PluginResult<()> {
        self.writes.lock().unwrap().push(value);
        if self.reject.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(PluginError::transport("simulated host save failure"));
        }
        Ok(())
    }
}
pub(crate) fn configured(site: Site, client: Arc<Client>) -> Autoclaw {
    let mut provider = Autoclaw::new(site);
    provider.client = Some(client);
    provider.sink = Arc::new(Sink::default());
    *provider.config.write().unwrap() = Some(Config {
        proxy: None,
        methods: json!([{"id":"oauth","type":"oauth"}]),
        subscriptions_enabled: true,
    });
    provider
}
pub(crate) fn credentials(site: Site) -> Value {
    json!({"type":"oauth","authMethod":"oauth","provider":site.id(),"access":"access-one","refresh":"refresh-one","expires":now()+DAY,"uid":"real-user","name":"Same name"})
}
pub(crate) const DAY: u64 = 86_400_000;
pub(crate) fn envelope(data: Value) -> (u16, Value) {
    (200, json!({"code":0,"data":data}))
}
pub(crate) async fn collect(mut response: FetchResponse) -> Vec<u8> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.body.next().await {
        bytes.extend(chunk.unwrap());
    }
    bytes
}

#[tokio::test]
async fn both_manifests_initialize_without_reading_sources_network_or_login_and_catalogs_are_honest(
) {
    for site in [Site::International, Site::China] {
        let client = Client::new(vec![]);
        let mut provider = Autoclaw::new(site);
        provider.client = Some(client.clone());
        let manifest: Value = serde_json::from_str(if site == Site::China {
            include_str!("../../autoclaw-cn/nora-provider.json")
        } else {
            include_str!("../nora-provider.json")
        })
        .unwrap();
        assert!(manifest.pointer("/config/connectionOptions").is_none());
        assert!(manifest.get("localSignIns").is_none());
        provider
            .initialize(InitializeParams {
                api_version: 1,
                subscription_accounts_enabled: true,
                plugin_id: site.id().into(),
                plugin_directory: "/fake".into(),
                data_directory: "/fake".into(),
                manifest,
                installed_manifest: None,
                runtime_config: Value::Null,
            })
            .await
            .unwrap();
        assert_eq!(
            provider.accounts(CallContext::default()).await.unwrap(),
            json!([])
        );
        assert_eq!(
            provider.models(CallContext::default()).await.unwrap()["source"],
            "catalog"
        );
        let mut context = CallContext::default();
        context.credentials = Some(credentials(site));
        let models = provider.models(context).await.unwrap();
        assert_eq!(models["source"], "catalog");
        assert_eq!(models["models"].as_array().unwrap().len(), 4);
        let ids = models["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| text(v, "id"))
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            [
                "zai_auto",
                "zai_auto-fast",
                "zaicoding_glm-5.3",
                "zai_glm-5.3-flash"
            ]
        );
        assert_eq!(
            models["models"][0]["contextWindow"],
            if site == Site::China { 1048576 } else { 200000 }
        );
        assert_eq!(
            models["models"][2]["maxTokens"],
            if site == Site::China { 307200 } else { 131072 }
        );
        assert_eq!(models["models"][0]["info"]["toolCalling"], true);
        if site == Site::China {
            assert_eq!(
                models["models"][0]["info"]["reasoningEfforts"],
                json!(["low", "high", "max"])
            );
            assert_eq!(models["models"][2]["input"], json!(["text"]));
            assert_eq!(models["models"][3]["input"], json!(["text", "image"]));
        }
        assert!(!models.to_string().contains("turbo"));
        for value in models["models"].as_array().unwrap() {
            let mut base = serde_json::to_value(nora_ai::Model::new(
                text(value, "id"),
                "openai-completions",
                site.id(),
            ))
            .unwrap();
            base.as_object_mut()
                .unwrap()
                .extend(value.as_object().unwrap().clone());
            let parsed: nora_ai::Model = serde_json::from_value(base).unwrap();
            assert_eq!(
                parsed.context_window,
                value["contextWindow"].as_u64().unwrap()
            );
            assert_eq!(parsed.max_tokens, value["maxTokens"].as_u64().unwrap());
            assert_eq!(parsed.info.tool_calling, Some(true));
            assert_eq!(parsed.info.pricing_known, Some(false));
        }
        assert!(client.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn subscription_disable_blocks_native_operations_before_any_network_or_listener() {
    let client = Client::new(vec![]);
    let provider = configured(Site::China, client.clone());
    let mut context = CallContext::default();
    context.credentials = Some(credentials(Site::China));
    context.subscription_accounts_enabled = false;
    assert_eq!(
        provider.models(context.clone()).await.unwrap_err().code,
        "subscription_accounts_disabled"
    );
    assert_eq!(
        provider.usage(context).await.unwrap_err().code,
        "subscription_accounts_disabled"
    );
    for method in ["checkin", "checkinStatus"] {
        assert_eq!(provider.call(method,json!({"subscriptionAccountsEnabled":false,"credentials":credentials(Site::China)})).await.unwrap_err().code,"subscription_accounts_disabled");
    }
    assert_eq!(
        provider
            .authorize(
                json!({"subscriptionAccountsEnabled":false,"inputs":{"phone":"13800138000"}})
            )
            .await
            .unwrap_err()
            .code,
        "subscription_accounts_disabled"
    );
    assert!(client.requests.lock().unwrap().is_empty());
    assert!(provider.pending.lock().await.is_empty());
}

#[tokio::test]
async fn account_metadata_uses_real_uid_and_region_not_display_name_or_rotating_tokens() {
    let client = Client::new(vec![]);
    let international = configured(Site::International, client.clone());
    let china = configured(Site::China, client.clone());
    let mut context = CallContext::default();
    context.credentials = Some(credentials(Site::International));
    let first = international.accounts(context.clone()).await.unwrap()["credentials"]
        ["accountIdentity"]
        .clone();
    let credentials = context.credentials.as_mut().unwrap();
    credentials["access"] = json!("rotated");
    credentials["name"] = json!("different");
    assert_eq!(
        international.accounts(context.clone()).await.unwrap()["credentials"]["accountIdentity"],
        first
    );
    context.credentials.as_mut().unwrap()["provider"] = json!("autoclaw-cn");
    assert_ne!(
        china.accounts(context.clone()).await.unwrap()["credentials"]["accountIdentity"],
        first
    );
    assert!(international.accounts(context).await.is_err());
    assert!(client.requests.lock().unwrap().is_empty());
}
