use async_trait::async_trait;
use nora_ai::{HttpClient, HttpClientError, HttpRequest, HttpStreamingResponse};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::Mutex,
};

pub struct Client {
    pub requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<(u16, Vec<u8>)>>,
}
impl Client {
    pub fn new(values: Vec<Value>) -> Self {
        Self {
            requests: Mutex::default(),
            replies: Mutex::new(
                values
                    .into_iter()
                    .map(|v| (200, serde_json::to_vec(&v).unwrap()))
                    .collect(),
            ),
        }
    }
    pub fn raw(status: u16, body: Vec<u8>) -> Self {
        Self {
            requests: Mutex::default(),
            replies: Mutex::new(VecDeque::from([(status, body)])),
        }
    }
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        let (status, bytes) = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected Vertex network request");
        Ok(HttpStreamingResponse {
            status,
            headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
            body: Box::pin(futures::stream::once(async move { Ok(bytes.into()) })),
        })
    }
}
pub struct Directory(pub PathBuf);
impl Directory {
    pub fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "nora-vertex-{}-{}-{}",
            module_path!().replace("::", "-"),
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    pub fn write(&self, value: &Value) -> PathBuf {
        let path = self.0.join("adc.json");
        std::fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        path
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub fn person() -> Value {
    json!({"type":"authorized_user","client_id":"fixture-client","client_secret":"fixture-secret","refresh_token":"fixture-refresh"})
}
pub fn config(path: PathBuf) -> crate::config::Config {
    let mut config =
        crate::config::Config::from_inputs(&json!({"project":"project-one","sourcePath":path}))
            .unwrap();
    config.identity = crate::credentials::read(&config).unwrap().identity;
    config
}
