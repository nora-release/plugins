use super::*;
use crate::{HttpByteStream, HttpClientError, HttpStreamingResponse};
use serde_json::json;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

/// Stands in for the vendor: answers each request with the next reply.
struct Client {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<Value>>,
}

impl Client {
    fn new(replies: Vec<Value>) -> Self {
        Self {
            requests: Mutex::default(),
            replies: Mutex::new(replies.into()),
        }
    }
}

#[async_trait::async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        let value = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("an unexpected request");
        let bytes = value.to_string().into_bytes();
        let body: HttpByteStream = Box::pin(futures::stream::once(async move { Ok(bytes) }));
        Ok(HttpStreamingResponse {
            status: 200,
            headers: BTreeMap::new(),
            body,
        })
    }
}

fn flow() -> DeviceFlow {
    DeviceFlow {
        vendor: "Acme",
        device_code_url: "https://acme.test/device/code",
        token_url: "https://acme.test/token",
        client_id: "c1",
        scope: "read",
        verification_url: "https://acme.test/device",
        headers: vec![("x-client", "1.0".into())],
    }
}

fn due() -> DevicePending {
    let now = Instant::now();
    DevicePending {
        device_code: "dc".into(),
        interval: Duration::from_secs(5),
        next: now,
        expires: now + Duration::from_secs(900),
    }
}

#[tokio::test]
async fn a_sign_in_starts_with_the_vendors_client() {
    let client = Client::new(vec![
        json!({"device_code": "dc", "user_code": "ABCD-1234", "interval": 5}),
    ]);
    let started = flow().start(&client).await.unwrap();
    assert_eq!(started.code, "ABCD-1234");
    // no page named: the vendor's own
    assert_eq!(started.url, "https://acme.test/device");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].url, "https://acme.test/device/code");
    assert_eq!(
        requests[0].headers.get("x-client").map(String::as_str),
        Some("1.0")
    );
    let body = String::from_utf8(requests[0].body.clone().unwrap()).unwrap();
    assert_eq!(body, "client_id=c1&scope=read");
}

#[tokio::test]
async fn polling_waits_out_the_vendors_interval() {
    let client = Client::new(vec![json!({"error": "authorization_pending"})]);
    let mut pending = due();
    assert!(flow().poll(&client, &mut pending).await.unwrap().is_none());
    assert!(flow().poll(&client, &mut pending).await.unwrap().is_none());
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn slow_down_lengthens_the_interval() {
    let client = Client::new(vec![json!({"error": "slow_down"})]);
    let mut pending = due();
    assert!(flow().poll(&client, &mut pending).await.unwrap().is_none());
    assert_eq!(pending.interval, Duration::from_secs(10));
}

#[tokio::test]
async fn the_code_typed_gives_the_tokens() {
    let client = Client::new(vec![json!({"access_token": "a1", "refresh_token": "r1"})]);
    let tokens = flow().poll(&client, &mut due()).await.unwrap().unwrap();
    assert_eq!(tokens["refresh_token"], "r1");
    let body = String::from_utf8(client.requests.lock().unwrap()[0].body.clone().unwrap()).unwrap();
    assert!(body.contains("device_code=dc"));
    assert!(body.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"));
}

#[tokio::test]
async fn a_refused_sign_in_says_so() {
    let client = Client::new(vec![json!({"error": "access_denied"})]);
    let error = flow().poll(&client, &mut due()).await.unwrap_err();
    assert!(
        error.to_string().contains("Acme sign-in was cancelled"),
        "{error}"
    );
}
