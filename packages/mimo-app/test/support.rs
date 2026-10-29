use crate::net::{Net, Reply, Request};
use async_trait::async_trait;
use nora_provider_plugin_sdk::{NetworkProxy, PluginResult};
use serde_json::Value;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};
pub(crate) struct Response {
    pub expected: String,
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub cookies: Vec<String>,
    pub bytes: Vec<u8>,
}
impl Response {
    pub(crate) fn cookie(mut self, value: &str) -> Self {
        self.cookies.push(value.into());
        self
    }
    pub(crate) fn header(mut self, key: &str, value: &str) -> Self {
        self.headers.insert(key.into(), value.into());
        self
    }
}
pub(crate) fn reply(expected: &str, status: u16, value: Value) -> Response {
    Response {
        expected: expected.into(),
        status,
        headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
        cookies: Vec::new(),
        bytes: serde_json::to_vec(&value).unwrap(),
    }
}
pub(crate) fn raw(expected: &str, status: u16, bytes: Vec<u8>) -> Response {
    Response {
        expected: expected.into(),
        status,
        headers: BTreeMap::new(),
        cookies: Vec::new(),
        bytes,
    }
}
pub(crate) struct Recorded {
    pub request: Request,
    pub proxy: Option<NetworkProxy>,
    pub timeout: Duration,
}
pub(crate) struct Fixture {
    pub requests: Mutex<Vec<Recorded>>,
    pub replies: Mutex<VecDeque<Response>>,
}
impl Fixture {
    pub(crate) fn new(replies: Vec<Response>) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            replies: Mutex::new(replies.into()),
        })
    }
}
#[async_trait]
impl Net for Fixture {
    async fn send(
        &self,
        request: Request,
        proxy: Option<&NetworkProxy>,
        timeout: Duration,
    ) -> PluginResult<Reply> {
        let mut replies = self.replies.lock().unwrap();
        let index = replies
            .iter()
            .position(|reply| request.url.contains(&reply.expected))
            .unwrap_or_else(|| panic!("Unexpected MiMo fixture request: {}", request.url));
        let response = replies.remove(index).unwrap();
        drop(replies);
        self.requests.lock().unwrap().push(Recorded {
            request,
            proxy: proxy.cloned(),
            timeout,
        });
        Ok(Reply {
            status: response.status,
            headers: response.headers,
            set_cookies: response.cookies,
            body: Box::pin(futures::stream::once(async move { Ok(response.bytes) })),
        })
    }
}
