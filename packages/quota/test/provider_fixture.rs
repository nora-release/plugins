use async_trait::async_trait;
use futures::stream;
use nora_ai::{
    HttpClient, HttpClientError, HttpRequest, HttpStreamingResponse, ResolvedProviderAuth,
};
use nora_provider::ProviderAdapter;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Mutex;

pub struct Client {
    pub requests: Mutex<Vec<HttpRequest>>,
    pub status: u16,
    pub body: Vec<u8>,
}
impl Client {
    pub fn json(value: &Value) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            status: 200,
            body: serde_json::to_vec(value).unwrap(),
        }
    }
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        Ok(HttpStreamingResponse {
            status: self.status,
            headers: BTreeMap::new(),
            body: Box::pin(stream::iter(vec![Ok(self.body.clone().into())])),
        })
    }
}
pub async fn verify(
    adapter: &dyn ProviderAdapter,
    base: &str,
    endpoint: &str,
    bearer: bool,
    payload: &Value,
) {
    let client = Client::json(payload);
    let mut auth = ResolvedProviderAuth {
        token: Some("private-api-key".into()),
        base_url: Some(base.into()),
        ..Default::default()
    };
    for base in [
        "https://unrelated.invalid/v1",
        "https://api.deepseek.com.evil/v1",
        "http://unrelated.invalid",
    ] {
        auth.base_url = Some(base.into());
        assert_eq!(
            adapter.usage(&auth, None, &client).await.unwrap()["available"],
            false
        );
    }
    assert!(client.requests.lock().unwrap().is_empty());
    auth.base_url = Some(base.into());
    let usage = adapter.usage(&auth, None, &client).await.unwrap();
    assert_eq!(usage["available"], true);
    assert!(!usage.to_string().contains("private-api-key"));
    assert!(!usage.to_string().contains("unexposed-private-field"));
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, endpoint);
    assert_eq!(
        requests[0].headers["authorization"],
        if bearer {
            "Bearer private-api-key"
        } else {
            "private-api-key"
        }
    );
    assert!(requests[0].body.is_none());
    drop(requests);
    for status in [401, 403, 410, 500] {
        let denied = Client {
            requests: Mutex::new(Vec::new()),
            status,
            body: br#"{"message":"private-api-key"}"#.to_vec(),
        };
        let error = adapter.usage(&auth, None, &denied).await.unwrap_err();
        assert!(!error.contains("private-api-key"));
    }
    let malformed = Client {
        requests: Mutex::new(Vec::new()),
        status: 200,
        body: b"private-api-key".to_vec(),
    };
    assert!(!adapter
        .usage(&auth, None, &malformed)
        .await
        .unwrap_err()
        .contains("private-api-key"));
}
