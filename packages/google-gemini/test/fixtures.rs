use base64::{engine::general_purpose::STANDARD, Engine};
use futures::{stream, StreamExt};
use nora_ai::{HttpClient, HttpClientError, HttpRequest, HttpStreamingResponse};
use nora_provider::definition::Definition;
use nora_provider_plugin_sdk::{FetchRequest, FetchResponse};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};

pub struct Client {
    pub requests: Mutex<Vec<HttpRequest>>,
    responses: Mutex<VecDeque<HttpStreamingResponse>>,
}
impl Client {
    pub fn new(values: Vec<Value>) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(
                values
                    .into_iter()
                    .map(|value| {
                        Self::response(200, BTreeMap::new(), serde_json::to_vec(&value).unwrap())
                    })
                    .collect(),
            ),
        }
    }
    pub fn response(
        status: u16,
        headers: BTreeMap<String, String>,
        bytes: Vec<u8>,
    ) -> HttpStreamingResponse {
        HttpStreamingResponse {
            status,
            headers,
            body: Box::pin(stream::once(async move { Ok(bytes.into()) })),
        }
    }
    pub fn push(&self, response: HttpStreamingResponse) {
        self.responses.lock().unwrap().push_back(response);
    }
}
#[async_trait::async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| HttpClientError::Transport("Unexpected request".into()))
    }
}
pub fn definition() -> Definition {
    let mut definition =
        Definition::parse(serde_json::from_str(include_str!("../nora-provider.json")).unwrap())
            .unwrap();
    definition.adapter = Arc::new(crate::provider::GoogleGemini);
    definition
}
pub fn request(path: &str, value: Value) -> FetchRequest {
    FetchRequest {
        subscription_accounts_enabled: true,
        account: "work".into(),
        credentials: Some(json!({"type":"api","key":"private-google-key"})),
        network_proxy: None,
        url: format!("https://generativelanguage.googleapis.com{path}"),
        method: "POST".into(),
        headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
        metadata: BTreeMap::from([("sessionID".into(), "private-session-id".into())]),
        body: Some(STANDARD.encode(serde_json::to_vec(&value).unwrap())),
        body_file: None,
    }
}
pub async fn response_json(response: FetchResponse) -> Value {
    let bytes = response
        .body
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .concat();
    serde_json::from_slice(&bytes).unwrap()
}
pub fn image_response() -> Value {
    json!({"candidates":[{"content":{"parts":[{"text":"private thought","thought":true},{"inlineData":{"mimeType":"image/png","data":"aGlkZGVu"},"thought":true},{"text":"A painted bird"},{"inlineData":{"mimeType":"image/png","data":"cGljdHVyZQ=="}}]}}],"usageMetadata":{"promptTokenCount":4,"candidatesTokenCount":8,"totalTokenCount":12}})
}
