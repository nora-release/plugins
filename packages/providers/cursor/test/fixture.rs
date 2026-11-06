use async_trait::async_trait;
use futures::stream;
use nora_ai::{HttpClient, HttpClientError, HttpRequest, HttpStreamingResponse};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};
pub struct Reply {
    pub status: u16,
    pub chunks: Vec<Vec<u8>>,
}
impl Reply {
    pub fn json(value: Value) -> Self {
        Self {
            status: 200,
            chunks: vec![serde_json::to_vec(&value).unwrap()],
        }
    }
    pub fn status(status: u16, value: Value) -> Self {
        Self {
            status,
            chunks: vec![serde_json::to_vec(&value).unwrap()],
        }
    }
}
pub struct Client {
    pub requests: Mutex<Vec<HttpRequest>>,
    pub replies: Mutex<VecDeque<Reply>>,
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
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected Cursor HTTP request");
        Ok(HttpStreamingResponse {
            status: reply.status,
            headers: BTreeMap::new(),
            body: Box::pin(stream::iter(reply.chunks.into_iter().map(|b| Ok(b.into())))),
        })
    }
}
pub fn picker() -> Value {
    json!({"models":[{"name":"claude","clientDisplayName":"Claude","supportsAgent":true,"contextTokenLimit":200000,"parameterDefinitions":[{"id":"context","parameterType":{"enumParameter":{"values":[{"value":"200k"},{"value":"1m"}]}}},{"id":"effort","parameterType":{"enumParameter":{"values":[{"value":"low"},{"value":"medium"},{"value":"high"},{"value":"extra-high"}]}}},{"id":"fast","parameterType":{"booleanParameter":{"values":[{"value":"false"},{"value":"true"}]}}}],"variants":[{"legacySlug":"claude-medium","isDefaultNonMaxConfig":true,"parameterValues":[{"id":"context","value":"200k"},{"id":"effort","value":"medium"},{"id":"fast","value":"false"}]},{"legacySlug":"claude-high-fast","isMaxMode":true,"parameterValues":[{"id":"context","value":"1m"},{"id":"effort","value":"high"},{"id":"fast","value":"true"}]}]}]})
}
pub fn credential() -> Value {
    json!({"type":"oauth","authMethod":"browser","access":"private-cursor-token","expires":0,"accountId":"one@example.test","plan":"Pro"})
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
pub fn chat() -> Value {
    json!({"model":"claude@200k","stream":true,"messages":[{"role":"system","content":"Only caller tools may run."},{"role":"user","content":"Use my echo tool."}],"tools":[{"type":"function","function":{"name":"echo","description":"Echo","parameters":{"type":"object","properties":{"value":{"type":"string"}}}}}]})
}
pub fn update(id: u32, body: crate::protobuf::Pb) -> Vec<u8> {
    crate::protobuf::frame(
        &crate::protobuf::Pb::new()
            .bytes(1, crate::protobuf::Pb::new().bytes(id, body))
            .0,
    )
}
