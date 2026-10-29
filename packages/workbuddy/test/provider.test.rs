use super::*;
use nora_ai::{HttpByteStream, HttpClientError};
use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;

pub(super) struct Client {
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
            .expect("Unexpected WorkBuddy network request");
        let (bytes, kind) = if let Some(raw) = value.get("raw").and_then(Value::as_str) {
            (raw.as_bytes().to_vec(), "text/event-stream")
        } else {
            (value.to_string().into_bytes(), "application/json")
        };
        let body: HttpByteStream = Box::pin(futures::stream::once(async move { Ok(bytes.into()) }));
        Ok(HttpStreamingResponse {
            status,
            headers: BTreeMap::from([("content-type".into(), kind.into())]),
            body,
        })
    }
}
pub(super) fn configured(site: Site, client: Arc<Client>) -> WorkBuddy {
    let mut provider = WorkBuddy::new(site);
    provider.client = Some(client);
    *provider.config.write().unwrap() = Some(Config {
        proxy: None,
        methods: json!([{"id":"oauth","type":"oauth"},{"id":"desktop","type":"oauth"}]),
    });
    provider
}
pub(super) fn data(value: Value) -> (u16, Value) {
    (200, json!({"code":0,"data":value}))
}

#[tokio::test]
async fn both_regions_initialize_without_discovering_or_importing_any_account() {
    for site in [Site::China, Site::International] {
        let client = Client::new(vec![]);
        let mut provider = WorkBuddy::new(site);
        provider.client = Some(client.clone());
        let manifest: Value = serde_json::from_str(if site == Site::China {
            include_str!("../nora-plugin.json")
        } else {
            include_str!("../../workbuddy-ai/nora-plugin.json")
        })
        .unwrap();
        let params = InitializeParams {
            subscription_accounts_enabled: true,
            installed_manifest: None,
            runtime_config: serde_json::Value::Null,
            api_version: 2,
            plugin_id: site.id().into(),
            plugin_directory: "/test/provider".into(),
            data_directory: "/test/provider-data".into(),
            manifest,
        };
        assert_eq!(provider.initialize(params).await.unwrap()["apiVersion"], 2);
        assert_eq!(
            provider
                .auth_methods(CallContext::default())
                .await
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            provider.accounts(CallContext::default()).await.unwrap(),
            json!([])
        );
        assert_eq!(client.requests.lock().unwrap().len(), 0);
    }
}

#[tokio::test]
async fn browser_sign_in_polls_tokens_then_the_account_and_keeps_each_region_separate() {
    for site in [Site::China, Site::International] {
        let client = Client::new(vec![
            data(json!({"state":"sign-in-state","authUrl":"https://login.tencent.com/auth"})),
            data(json!({"accessToken":"access","refreshToken":"refresh","expiresIn":3600})),
            data(json!({"uid":"user-1","nickname":"Member","type":"personal"})),
        ]);
        let provider = configured(site, client.clone());
        let flow = provider.authorize(json!({"method":"oauth"})).await.unwrap();
        assert!(flow["url"].as_str().unwrap().contains("version=2.0.0"));
        let result = provider
            .callback(json!({"session":flow["session"]}))
            .await
            .unwrap();
        assert_eq!(result["credentials"]["uid"], "user-1");
        assert_eq!(result["credentials"]["refresh"], "refresh");
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(requests
            .iter()
            .all(|request| request.url.starts_with(site.endpoint())));
        assert!(requests[0]
            .url
            .ends_with(&format!("platform={}", site.id())));
        assert_eq!(
            requests[2].headers.get("authorization").map(String::as_str),
            Some("Bearer access")
        );
        assert!(requests[1].headers.get("authorization").is_none());
    }
}

#[tokio::test]
async fn refresh_uses_the_refresh_token_once_and_waits_for_the_host_save_acknowledgment() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let client = Client::new(vec![data(
        json!({"accessToken":"new-access","refreshToken":"new-refresh","expiresIn":3600}),
    )]);
    let provider = Arc::new(configured(Site::China, client.clone()));
    let (host, plugin) = tokio::io::duplex(64 * 1024);
    let (input, output) = tokio::io::split(plugin);
    let task = tokio::spawn(nora_provider_plugin_sdk::serve_io(provider, input, output));
    let (input, mut output) = tokio::io::split(host);
    let mut input = BufReader::new(input);
    let mut line = String::new();
    let manifest: Value = serde_json::from_str(include_str!("../nora-plugin.json")).unwrap();
    output.write_all(format!("{}\n",json!({"id":1,"method":"initialize","params":{"apiVersion":2,"pluginID":"workbuddy","pluginDirectory":"/test/plugin","dataDirectory":"/test/data","manifest":manifest,"hostCapabilities":{"credentialUpdates":true}}})).as_bytes()).await.unwrap();
    input.read_line(&mut line).await.unwrap();
    assert!(serde_json::from_str::<Value>(&line)
        .unwrap()
        .get("result")
        .is_some());
    line.clear();
    output.write_all(format!("{}\n",json!({"id":2,"method":"refresh","params":{"credentials":{"type":"oauth","uid":"member","access":"old-access","refresh":"old-refresh","expires":1}}})).as_bytes()).await.unwrap();
    input.read_line(&mut line).await.unwrap();
    let event: Value = serde_json::from_str(&line).unwrap();
    line.clear();
    assert_eq!(event["event"], "credentials");
    assert_eq!(event["credentials"]["refresh"], "new-refresh");
    assert!(
        tokio::time::timeout(Duration::from_millis(25), input.read_line(&mut line))
            .await
            .is_err()
    );
    output.write_all(format!("{}\n",json!({"id":0,"method":"credentialsSaved","params":{"id":2,"updateID":event["updateID"],"success":true}})).as_bytes()).await.unwrap();
    input.read_line(&mut line).await.unwrap();
    let result: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(result["result"]["credentials"]["access"], "new-access");
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    drop(output);
    drop(input);
    task.abort();
}

#[tokio::test]
async fn concurrent_refreshes_reuse_the_new_pair_instead_of_spending_a_token_twice() {
    let client = Client::new(vec![data(
        json!({"accessToken":"next","refreshToken":"next-refresh","expiresIn":3600}),
    )]);
    let provider = configured(Site::China, client.clone());
    let old = json!({"type":"oauth","access":"old","refresh":"one-use","uid":"member"});
    let (first, second) = tokio::join!(
        auth::renew(&provider, client.as_ref(), &old),
        auth::renew(&provider, client.as_ref(), &old)
    );
    assert_eq!(first.unwrap()["access"], "next");
    assert_eq!(second.unwrap()["refresh"], "next-refresh");
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[test]
fn desktop_sources_are_read_only_and_cannot_drift_to_a_different_account() {
    let path = std::env::temp_dir().join(format!("nora-workbuddy-source-{}.json", random_id()));
    let original=json!({"auth":{"accessToken":"desktop-token","refreshToken":"desktop-refresh","expiresAt":now()+3_600_000},"account":{"uid":"first","nickname":"First"}}).to_string();
    std::fs::write(&path, &original).unwrap();
    let marker = json!({"source":"desktop","sourcePath":path,"sourceUID":"first"});
    assert_eq!(
        auth::read_source(Site::China, &marker).unwrap()["access"],
        "desktop-token"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    let mut changed: Value = serde_json::from_str(&original).unwrap();
    changed["account"]["uid"] = json!("second");
    std::fs::write(&path, changed.to_string()).unwrap();
    assert!(auth::read_source(Site::China, &marker).is_err());
    changed["auth"]["accessToken"] = json!({"$wbEncrypted":1,"envelope":"opaque"});
    std::fs::write(&path, changed.to_string()).unwrap();
    assert!(auth::read_file(&path)
        .unwrap_err()
        .message
        .contains("encrypts"));
    std::fs::remove_file(path).unwrap();
}
