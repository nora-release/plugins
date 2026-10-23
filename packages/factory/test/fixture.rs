use async_trait::async_trait;
use nora_ai::{HttpClient, HttpClientError, HttpRequest, HttpStreamingResponse};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};
pub struct Reply {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub chunks: Vec<Vec<u8>>,
}
impl Reply {
    pub fn json(value: Value) -> Self {
        Self {
            status: 200,
            headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
            chunks: vec![value.to_string().into_bytes()],
        }
    }
    pub fn status(status: u16, value: Value) -> Self {
        Self {
            status,
            ..Self::json(value)
        }
    }
    pub fn sse(values: &[Value]) -> Self {
        let bytes = values
            .iter()
            .map(|v| format!("data: {v}\n\n"))
            .collect::<String>()
            .into_bytes();
        Self {
            status: 200,
            headers: BTreeMap::from([("content-type".into(), "text/event-stream".into())]),
            chunks: bytes.chunks(3).map(<[u8]>::to_vec).collect(),
        }
    }
    pub fn response(self) -> HttpStreamingResponse {
        HttpStreamingResponse {
            status: self.status,
            headers: self.headers,
            body: Box::pin(futures::stream::iter(
                self.chunks.into_iter().map(|b| Ok(b.into())),
            )),
        }
    }
}
pub struct Client {
    pub requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<Reply>>,
}
impl Client {
    pub fn new(replies: Vec<Reply>) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            replies: Mutex::new(replies.into()),
        })
    }
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected Factory request")
            .response())
    }
}
pub fn saved() -> Value {
    json!({"type":"oauth","authMethod":"device","access":"private-old-access","refresh":"private-old-refresh","expires":4102444800000u64,"userId":"user-one","email":"one@example.test","orgId":"workos-one","activeOrganizationId":"factory-one"})
}
pub fn key() -> Value {
    json!({"type":"api","key":"fk-private","metadata":{"userId":"user-one","email":"one@example.test","region":"us"}})
}
pub fn me() -> Value {
    json!({"userId":"user-one","email":"one@example.test","orgId":"factory-one","region":"us","premBaseHostV2":""})
}
pub fn token() -> Value {
    use base64::Engine;
    let claims=base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json!({"sub":"user-one","email":"one@example.test","exp":4102444800u64,"org_id":"workos-one"}).to_string());
    json!({"access_token":format!("e30.{claims}.signature"),"refresh_token":"private-new-refresh","organization_id":"workos-one"})
}
pub fn fetch(path: &str, body: Value) -> nora_provider_plugin_sdk::FetchRequest {
    use base64::Engine;
    nora_provider_plugin_sdk::FetchRequest {
        account: "one".into(),
        credentials: Some(saved()),
        network_proxy: None,
        url: format!("https://api.factory.ai{path}"),
        method: "POST".into(),
        headers: BTreeMap::from([
            ("authorization".into(), "Bearer caller-token".into()),
            ("cookie".into(), "caller-cookie".into()),
        ]),
        metadata: BTreeMap::new(),
        body: Some(base64::engine::general_purpose::STANDARD.encode(body.to_string())),
        body_file: None,
    }
}
pub async fn body(response: nora_provider_plugin_sdk::FetchResponse) -> String {
    use futures::StreamExt;
    String::from_utf8(
        response
            .body
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .concat(),
    )
    .unwrap()
}
