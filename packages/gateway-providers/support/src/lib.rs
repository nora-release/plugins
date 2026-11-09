//! The HTTP client, requests and errors the provider packages of Nora Router
//! were written against, on reqwest and plugin-kit's provider SDK, so their
//! vendor logic carries over to nora-gateway's plugins unchanged.

mod browser_sign_in;
mod oauth_callback_server;

pub use browser_sign_in::BrowserSignIn;
pub use oauth_callback_server::{
    OAuthCallbackInput, OAuthCallbackServer, OAuthCallbackServerConfig,
};

use futures::{Stream, StreamExt};
use nora_plugin_runtime_sdk::protocol::RpcError;
use nora_plugin_runtime_sdk::provider::{rate_limited, sign_in_needed};
use std::collections::BTreeMap;
use std::pin::Pin;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
}

#[derive(Clone, Debug)]
pub struct HttpRequest {
    pub method: HttpMethod,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Option<Vec<u8>>,
}

impl HttpRequest {
    pub fn new(method: HttpMethod, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: BTreeMap::new(),
            body: None,
        }
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    pub fn with_body(mut self, body: Vec<u8>) -> Self {
        self.body = Some(body);
        self
    }

    /// The value as the body, sent as JSON unless a type is already set.
    pub fn with_json_body(mut self, value: &serde_json::Value) -> Result<Self, serde_json::Error> {
        self.body = Some(serde_json::to_vec(value)?);
        self.headers
            .entry("content-type".into())
            .or_insert_with(|| "application/json".into());
        Ok(self)
    }
}

/// `bytes` random bytes from the operating system, as lowercase hex.
pub fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    getrandom::getrandom(&mut buffer).expect("The operating system random source is unavailable");
    buffer.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Debug)]
pub enum HttpClientError {
    Transport(String),
}

impl std::fmt::Display for HttpClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self::Transport(message) = self;
        formatter.write_str(message)
    }
}

pub type HttpByteStream =
    Pin<Box<dyn Stream<Item = Result<Vec<u8>, HttpClientError>> + Send + 'static>>;

pub struct HttpStreamingResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: HttpByteStream,
}

#[async_trait::async_trait]
pub trait HttpClient: Send + Sync {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError>;
}

/// The client requests go out with, through the account's proxy when it
/// has one.
pub struct ReqwestHttpClient(reqwest::Client);

impl ReqwestHttpClient {
    pub fn new(proxy: Option<&str>) -> PluginResult<Self> {
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(5));
        if let Some(proxy) = proxy.filter(|proxy| !proxy.is_empty()) {
            let proxy = reqwest::Proxy::all(proxy)
                .map_err(|_| PluginError::invalid("The account's proxy is invalid."))?;
            builder = builder.proxy(proxy);
        }
        builder
            .build()
            .map(Self)
            .map_err(|error| PluginError::transport(error.to_string()))
    }
}

#[async_trait::async_trait]
impl HttpClient for ReqwestHttpClient {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        let method = match request.method {
            HttpMethod::Get => reqwest::Method::GET,
            HttpMethod::Post => reqwest::Method::POST,
        };
        let mut builder = self.0.request(method, &request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }
        let response = builder
            .send()
            .await
            .map_err(|error| HttpClientError::Transport(error.to_string()))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_owned(), value.to_owned()))
            })
            .collect();
        let body = response.bytes_stream().map(|chunk| {
            chunk
                .map(|bytes| bytes.to_vec())
                .map_err(|error| HttpClientError::Transport(error.to_string()))
        });
        Ok(HttpStreamingResponse {
            status,
            headers,
            body: Box::pin(body),
        })
    }
}

/// Reads a body, keeping at most `limit` bytes, as text.
pub async fn collect_body_text_limited(
    mut body: HttpByteStream,
    limit: usize,
) -> Result<String, HttpClientError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        let room = limit.saturating_sub(bytes.len());
        bytes.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if chunk.len() > room {
            return Err(HttpClientError::Transport("The reply is too large.".into()));
        }
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// An error as the protocol modules name it, with the old SDK's codes;
/// [`From`] turns it into what the router reads, a sign-in to renew or a
/// rest among them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginError {
    pub code: String,
    pub message: String,
}

pub type PluginResult<T> = Result<T, PluginError>;

impl PluginError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_request", message)
    }
    pub fn authentication(message: impl Into<String>) -> Self {
        Self::new("authentication_required", message)
    }
    pub fn transport(message: impl Into<String>) -> Self {
        Self::new("upstream_transport_error", message)
    }
    pub fn unsupported(what: impl std::fmt::Display) -> Self {
        Self::new(
            "method_not_supported",
            format!("This provider plugin does not support {what}."),
        )
    }
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl From<PluginError> for RpcError {
    fn from(error: PluginError) -> Self {
        match error.code.as_str() {
            "authentication_required" => sign_in_needed(error.message),
            "rate_limited" => rate_limited(error.message, None),
            "invalid_request" => RpcError::invalid_params(error.message),
            "method_not_supported" => RpcError::method_not_found(),
            _ => RpcError::new(-32000, error.message),
        }
    }
}

/// A request the router hands the plugin to send: its body read whole.
pub struct FetchRequest {
    pub credentials: Option<serde_json::Value>,
    pub proxy: Option<String>,
    pub url: String,
    pub method: String,
    pub metadata: BTreeMap<String, String>,
    pub body: Option<Vec<u8>>,
}

impl FetchRequest {
    pub fn decode_body(&self) -> PluginResult<Option<Vec<u8>>> {
        Ok(self.body.clone())
    }
}

pub type ResponseBody = Pin<Box<dyn Stream<Item = PluginResult<Vec<u8>>> + Send + 'static>>;

/// A response for the router: its head, and its body as it arrives.
pub struct FetchResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: ResponseBody,
}

impl FetchResponse {
    pub fn bytes(status: u16, headers: BTreeMap<String, String>, body: Vec<u8>) -> Self {
        Self {
            status,
            headers,
            body: Box::pin(futures::stream::once(async move { Ok(body) })),
        }
    }
}

/// Why an OAuth sign-in through the callback server stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OAuthError {
    Transport(String),
    InvalidResponse(String),
    Cancelled,
}

impl std::fmt::Display for OAuthError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(message) | Self::InvalidResponse(message) => {
                formatter.write_str(message)
            }
            Self::Cancelled => formatter.write_str("The sign-in was cancelled."),
        }
    }
}

impl std::error::Error for OAuthError {}
