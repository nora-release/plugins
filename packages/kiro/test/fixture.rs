use async_trait::async_trait;
use futures::stream;
use nora_ai::{HttpClient, HttpClientError, HttpRequest, HttpStreamingResponse};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Mutex,
};
pub const PROFILE: &str = "arn:aws:codewhisperer:eu-central-1:123456789012:profile/example";
pub struct Reply {
    pub status: u16,
    pub chunks: Vec<Vec<u8>>,
}
impl Reply {
    pub fn json(value: Value) -> Self {
        Self::raw(200, serde_json::to_vec(&value).unwrap())
    }
    pub fn raw(status: u16, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            chunks: vec![bytes.into()],
        }
    }
}
pub struct Client {
    pub requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<Reply>>,
}
impl Client {
    pub fn new(replies: Vec<Reply>) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            replies: Mutex::new(replies.into()),
        }
    }
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected Kiro request");
        Ok(HttpStreamingResponse {
            status: reply.status,
            headers: BTreeMap::new(),
            body: Box::pin(stream::iter(
                reply.chunks.into_iter().map(|bytes| Ok(bytes.into())),
            )),
        })
    }
}
pub fn usage() -> Value {
    json!({"userInfo":{"email":"person@example.test"},"subscriptionInfo":{"subscriptionTitle":"KIRO PRO"},"nextDateReset":1800000000,"usageBreakdownList":[{"displayNamePlural":"Credits","usageLimitWithPrecision":1000.0,"currentUsageWithPrecision":25.0}]})
}
pub fn event(kind: &str, value: Value) -> Vec<u8> {
    headers(&[(":message-type", "event"), (":event-type", kind)], value)
}
pub fn headers(headers: &[(&str, &str)], value: Value) -> Vec<u8> {
    let payload = serde_json::to_vec(&value).unwrap();
    let mut encoded = Vec::new();
    for (name, value) in headers {
        encoded.push(name.len() as u8);
        encoded.extend_from_slice(name.as_bytes());
        encoded.push(7);
        encoded.extend_from_slice(&(value.len() as u16).to_be_bytes());
        encoded.extend_from_slice(value.as_bytes());
    }
    let total = 16 + encoded.len() + payload.len();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(total as u32).to_be_bytes());
    bytes.extend_from_slice(&(encoded.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&crc32fast::hash(&bytes).to_be_bytes());
    bytes.extend(encoded);
    bytes.extend(payload);
    bytes.extend_from_slice(&crc32fast::hash(&bytes).to_be_bytes());
    bytes
}
pub async fn body(response: nora_provider_plugin_sdk::FetchResponse) -> String {
    use futures::StreamExt;
    let bytes = response
        .body
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .concat();
    String::from_utf8(bytes).unwrap()
}
pub fn saved() -> Value {
    json!({"type":"oauth","method":"social","access":"private-access","refresh":"private-refresh","expires":crate::session::now()+3600000,"region":"eu-west-1","profileArn":PROFILE,"email":"person@example.test","accountId":crate::session::fingerprint("kiro-account\0person@example.test\0")})
}
