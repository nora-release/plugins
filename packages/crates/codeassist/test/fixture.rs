use async_trait::async_trait;
use futures::stream;
use nora_ai::{HttpClient, HttpClientError, HttpRequest, HttpStreamingResponse};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
}
impl Reply {
    pub fn json(value: Value) -> Self {
        Self {
            status: 200,
            body: serde_json::to_vec(&value).unwrap(),
        }
    }
    pub fn raw(status: u16, body: &[u8]) -> Self {
        Self {
            status,
            body: body.to_vec(),
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
        let version = request.url == crate::VERSION;
        self.requests.lock().unwrap().push(request);
        if version {
            return Ok(HttpStreamingResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: Box::pin(stream::iter(vec![Ok(b"version: '3.0.1'\n"
                    .to_vec()
                    .into())])),
            });
        }
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected Google request in fixture");
        Ok(HttpStreamingResponse {
            status: reply.status,
            headers: BTreeMap::new(),
            body: Box::pin(stream::iter(vec![Ok(reply.body.into())])),
        })
    }
}
