use nora_ai::{HttpClient, HttpClientError, HttpRequest, HttpStreamingResponse};
use serde_json::Value;
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
};
pub(crate) struct Reply {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}
#[derive(Default)]
pub(crate) struct Client {
    pub requests: Mutex<Vec<HttpRequest>>,
    pub replies: Mutex<VecDeque<Reply>>,
}
impl Client {
    pub fn push(&self, status: u16, value: Value) {
        self.raw(status, BTreeMap::new(), value.to_string().into_bytes());
    }
    pub fn raw(&self, status: u16, headers: BTreeMap<String, String>, body: Vec<u8>) {
        self.replies.lock().unwrap().push_back(Reply {
            status,
            headers,
            body,
        });
    }
}
#[async_trait::async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected remote HTTP request");
        Ok(HttpStreamingResponse {
            status: reply.status,
            headers: reply.headers,
            body: Box::pin(futures::stream::once(async move { Ok(reply.body.into()) })),
        })
    }
}
pub(crate) struct Directory(pub PathBuf);
impl Directory {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!(
            "nora-remote-provider-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
