use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use nora_ai::{HttpClient, HttpClientError, HttpRequest, HttpStreamingResponse};
use serde_json::Value;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

pub(crate) fn jwt(value: Value) -> String {
    format!(
        "e30.{}.signature",
        URL_SAFE_NO_PAD.encode(value.to_string())
    )
}
#[derive(Clone)]
pub(crate) struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
    pub headers: BTreeMap<String, String>,
    pub delay: Duration,
    pub pending: bool,
}
impl Reply {
    pub(crate) fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            body: body.to_string().into_bytes(),
            headers: BTreeMap::new(),
            delay: Duration::ZERO,
            pending: false,
        }
    }
    pub(crate) fn raw(status: u16, body: &str) -> Self {
        Self {
            status,
            body: body.as_bytes().to_vec(),
            headers: BTreeMap::new(),
            delay: Duration::ZERO,
            pending: false,
        }
    }
}
#[derive(Default)]
pub(crate) struct Client {
    pub requests: Mutex<Vec<HttpRequest>>,
    pub replies: Mutex<VecDeque<Reply>>,
}
impl Client {
    pub(crate) fn new(replies: Vec<Reply>) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            replies: Mutex::new(replies.into()),
        })
    }
    pub(crate) fn push(&self, status: u16, body: Value) {
        self.replies
            .lock()
            .unwrap()
            .push_back(Reply::json(status, body));
    }
    pub(crate) fn form(&self, index: usize) -> BTreeMap<String, String> {
        url::form_urlencoded::parse(self.requests.lock().unwrap()[index].body.as_ref().unwrap())
            .into_owned()
            .collect()
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
            .expect("Unexpected OAuth provider request");
        if !reply.delay.is_zero() {
            tokio::time::sleep(reply.delay).await;
        }
        let stream = futures::stream::once(async move { Ok(reply.body.into()) });
        Ok(HttpStreamingResponse {
            status: reply.status,
            headers: reply.headers,
            body: if reply.pending {
                Box::pin(futures::StreamExt::chain(
                    stream,
                    futures::stream::pending(),
                ))
            } else {
                Box::pin(stream)
            },
        })
    }
}
pub(crate) struct Directory(pub std::path::PathBuf);
impl Directory {
    pub(crate) fn new() -> Self {
        let path = std::env::temp_dir().join(format!("nora-oauth-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
