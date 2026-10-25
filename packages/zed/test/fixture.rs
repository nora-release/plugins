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
            headers: BTreeMap::new(),
            chunks: vec![value.to_string().into_bytes()],
        }
    }
    pub fn status(status: u16) -> Self {
        Self {
            status,
            ..Self::json(json!({"message":"private-reflected-token prompt"}))
        }
    }
    pub fn lines(values: &[Value], wrapped: bool) -> Self {
        let bytes = values
            .iter()
            .map(|v| format!("{v}\n"))
            .collect::<String>()
            .into_bytes();
        Self {
            status: 200,
            headers: if wrapped {
                BTreeMap::from([(
                    "x-zed-server-supports-status-messages".into(),
                    "true".into(),
                )])
            } else {
                BTreeMap::new()
            },
            chunks: bytes.chunks(7).map(<[u8]>::to_vec).collect(),
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
            .expect("Unexpected Zed HTTP request")
            .response())
    }
}
pub fn me() -> Value {
    json!({"user":{"legacy_user_id":77,"github_login":"actual-login"},"organizations":[{"id":"personal","is_personal":true},{"id":"team"}],"default_organization_id":"team","plans_by_organization":{"personal":"zed_student"},"plan":{"plan_v3":"zed_student","subscription_period":{"ended_at":"2030-01-02T03:04:05Z"}}})
}
pub fn saved(org: &str) -> Value {
    json!({"type":"oauth","authMethod":"browser","userID":"77","access":"private-zed-access","systemID":"private-system-id","organizationID":org,"plan":"zed_student","label":"actual-login","accountId":"77"})
}
pub fn models() -> Value {
    json!({"models":[{"provider":"anthropic","id":"claude-model","display_name":"Claude","max_token_count":200000,"max_output_tokens":64000,"supports_images":true,"supports_thinking":true,"supported_effort_levels":[{"value":"low"},{"value":"high"}]},{"provider":"open_ai","id":"gpt-model","max_token_count":400000,"max_output_tokens":128000},{"provider":"google","id":"gemini-model","max_token_count":1048576,"max_output_tokens":65536},{"provider":"x_ai","id":"grok-model","max_token_count":500000,"max_output_tokens":64000}]})
}
pub fn anthropic() -> Vec<Value> {
    vec![
        json!({"event":{"type":"message_start","message":{"id":"m1","type":"message","role":"assistant","model":"claude-model","content":[],"usage":{"input_tokens":12,"output_tokens":0}}}}),
        json!({"event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"original-tool-id","name":"original_name","input":{}}}}),
        json!({"event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"x\":"}}}),
        json!({"event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"1}"}}}),
        json!({"event":{"type":"content_block_stop","index":0}}),
        json!({"event":{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":2}}}),
        json!({"event":{"type":"message_stop"}}),
        json!({"status":"stream_ended"}),
    ]
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
pub fn fetch(path: &str, body: Value) -> nora_provider_plugin_sdk::FetchRequest {
    use base64::Engine;
    nora_provider_plugin_sdk::FetchRequest {
        subscription_accounts_enabled: true,
        account: "one".into(),
        credentials: Some(saved("personal")),
        network_proxy: None,
        url: format!("https://cloud.zed.dev{path}"),
        method: "POST".into(),
        headers: BTreeMap::from([
            ("authorization".into(), "Bearer must-not-forward".into()),
            ("cookie".into(), "must-not-forward".into()),
        ]),
        metadata: BTreeMap::new(),
        body: Some(base64::engine::general_purpose::STANDARD.encode(body.to_string())),
        body_file: None,
    }
}
