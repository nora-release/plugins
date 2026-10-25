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
            .expect("Unexpected CodeArts request");
        let (bytes, kind) = match value.get("raw").and_then(Value::as_str) {
            Some(raw) => (raw.as_bytes().to_vec(), "text/event-stream"),
            None => (value.to_string().into_bytes(), "application/json"),
        };
        let mut headers = BTreeMap::from([("content-type".into(), kind.into())]);
        for (name, value) in value
            .get("headers")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            headers.insert(name.clone(), value.as_str().unwrap().into());
        }
        let first = futures::stream::once(async move { Ok(bytes.into()) });
        let body: HttpByteStream = if value.get("pending").and_then(Value::as_bool) == Some(true) {
            Box::pin(futures::StreamExt::chain(first, futures::stream::pending()))
        } else {
            Box::pin(first)
        };
        Ok(HttpStreamingResponse {
            status,
            headers,
            body,
        })
    }
}
pub(crate) fn configured(client: Arc<Client>) -> CodeArts {
    let mut provider = CodeArts::default();
    provider.client = Some(client);
    *provider.config.write().unwrap() = Some(Config {
        proxy: None,
        methods: json!([{"id":"oauth","type":"oauth"}]),
        data: std::env::temp_dir().join(format!("codearts-test-{}", crate::sign::id(16).unwrap())),
        max_inflight: 3,
    });
    provider
}
pub(crate) fn credentials() -> Value {
    json!({"access_key_id":"AK","secret_access_key":"SK","security_token":"ST","expires_at":"2099-01-01T00:00:00Z","refresh_token":"refresh","code_verifier":"verifier","dpop_private_key_jwk":crate::sign::key(),"session_id":"one-account-session","user_id":"member","user_name":"Member"})
}
pub(crate) fn data(value: Value) -> (u16, Value) {
    (200, json!({"code":0,"data":value}))
}

#[tokio::test]
async fn initialize_never_imports_or_authenticates_an_account() {
    let client = Client::new(vec![]);
    let mut provider = CodeArts::default();
    provider.client = Some(client.clone());
    let manifest: Value = serde_json::from_str(include_str!("../nora-provider.json")).unwrap();
    assert_eq!(manifest["config"]["initialResponseTimeoutMs"], 480000);
    assert_eq!(manifest["config"]["accountActions"], json!(["checkin"]));
    provider
        .initialize(InitializeParams {
            subscription_accounts_enabled: true,
            installed_manifest: None,
            runtime_config: serde_json::Value::Null,
            api_version: 1,
            plugin_id: "codearts".into(),
            plugin_directory: "/test/provider".into(),
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

#[test]
fn account_identity_reads_huawei_jwt_profile_and_session_survives_key_rotation() {
    use base64::Engine;
    let profile = json!({"principal_urn":"urn:member","user_name":"member@example.com"});
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(json!({"user_profile":profile.to_string()}).to_string());
    let mut credentials = credentials();
    credentials["refresh_token"] = json!(format!("header.{payload}.signature"));
    assert_eq!(
        identity(&credentials),
        ("urn:member".into(), "member@example.com".into())
    );
    let encoded_profile = base64::engine::general_purpose::STANDARD.encode(profile.to_string());
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(json!({"user_profile":encoded_profile}).to_string());
    credentials["refresh_token"] = json!(format!("header.{payload}.signature"));
    assert_eq!(
        identity(&credentials),
        ("urn:member".into(), "member@example.com".into())
    );
    let before = session(&credentials);
    credentials["access_key_id"] = json!("new-key");
    assert_eq!(session(&credentials), before);
    assert!(encode(json!({"expires_at":"invalid"})).is_err());
}
