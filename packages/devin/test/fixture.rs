use crate::protobuf::Pb;
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
    pub fn bytes(bytes: Vec<u8>) -> Self {
        Self {
            status: 200,
            headers: BTreeMap::new(),
            chunks: vec![bytes],
        }
    }
    pub fn json(value: Value) -> Self {
        Self {
            headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
            ..Self::bytes(value.to_string().into_bytes())
        }
    }
    pub fn status(status: u16, value: Value) -> Self {
        Self {
            status,
            ..Self::json(value)
        }
    }
    pub fn stream(frames: &[Pb], end: Option<Value>) -> Self {
        let mut bytes = Vec::new();
        for frame in frames {
            bytes.extend_from_slice(&crate::protobuf::frame(&frame.0).unwrap());
        }
        if let Some(end) = end {
            let value = end.to_string().into_bytes();
            bytes.push(2);
            bytes.extend_from_slice(&(value.len() as u32).to_be_bytes());
            bytes.extend_from_slice(&value);
        }
        Self {
            chunks: bytes.chunks(3).map(<[u8]>::to_vec).collect(),
            ..Self::bytes(Vec::new())
        }
    }
    pub fn response(self) -> HttpStreamingResponse {
        HttpStreamingResponse {
            status: self.status,
            headers: self.headers,
            body: Box::pin(futures::stream::iter(
                self.chunks.into_iter().map(|v| Ok(v.into())),
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
            .expect("Unexpected Devin request")
            .response())
    }
}
pub fn me() -> Value {
    json!({"userStatus":{"email":"one@example.test","name":"one","teamsTier":"TEAMS_TIER_DEVIN_PRO","planStatus":{"planInfo":{"planName":"Pro","billingStrategy":"BILLING_STRATEGY_QUOTA"},"dailyQuotaRemainingPercent":100,"weeklyQuotaResetAtUnix":"1893456000","planEnd":"2030-01-01T00:00:00Z","overageBalanceMicros":"1250000"}}})
}
pub fn saved() -> Value {
    json!({"type":"api","authMethod":"browser","key":"private-devin-key","accountId":"one@example.test","metadata":{"email":"one@example.test","plan":"Devin Pro","server":"https://server.codeium.com"}})
}
pub fn configs() -> Vec<u8> {
    let row = |id: &str, family: &str, image: bool, free: bool| {
        Pb::new()
            .text(1, id)
            .text(22, id)
            .int(5, u64::from(image))
            .int(24, if free { 4 } else { 2 })
            .int(18, 200000)
            .bytes(23, Pb::new().int(13, 64000).text(23, family))
            .bytes(30, Pb::new().text(1, family))
    };
    Pb::new()
        .bytes(1, row("swe-2-high", "swe-2", true, true))
        .bytes(1, row("swe-2-medium", "swe-2", true, true))
        .bytes(
            1,
            row("claude-opus-5-5-low", "claude-opus-5-5", true, false),
        )
        .bytes(
            1,
            row("claude-opus-5-5-high", "claude-opus-5-5", true, false),
        )
        .bytes(
            1,
            row("claude-opus-5-5-high-fast", "claude-opus-5-5", true, false),
        )
        .bytes(1, row("no-image-high", "no-image", false, false))
        .0
}
pub fn fetch(body: Value) -> nora_provider_plugin_sdk::FetchRequest {
    use base64::Engine;
    nora_provider_plugin_sdk::FetchRequest {
        subscription_accounts_enabled: true,
        account: "one".into(),
        credentials: Some(saved()),
        network_proxy: None,
        url: "https://server.codeium.com/devin/v1/chat/completions".into(),
        method: "POST".into(),
        headers: BTreeMap::from([
            ("authorization".into(), "Bearer caller-private".into()),
            ("cookie".into(), "private-cookie".into()),
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
pub fn temp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("nora-devin-{}-{name}", uuid::Uuid::new_v4()))
}
