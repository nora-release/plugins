use nora_ai::oauth::OAuthError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const XAI_ACCOUNTS_ORIGIN: &str = "https://accounts.x.ai";

// MARK: - Configuration

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthCallbackServerConfig {
    pub host: String,
    pub port: u16,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_host: Option<String>,
}

impl OAuthCallbackServerConfig {
    pub fn new(port: u16) -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port,
            path: "/callback".to_string(),
            redirect_host: None,
        }
    }

    pub fn with_path(port: u16, path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            ..Self::new(port)
        }
    }

    pub fn with_redirect_host(mut self, host: impl Into<String>) -> Self {
        self.redirect_host = Some(host.into());
        self
    }

    pub fn redirect_uri(&self) -> String {
        let host = self.redirect_host.as_deref().unwrap_or(&self.host);
        format!("http://{}:{}{}", host, self.port, self.path)
    }
}

impl Default for OAuthCallbackServerConfig {
    fn default() -> Self {
        Self::new(0)
    }
}

// MARK: - Server

pub struct OAuthCallbackServer {
    pub config: OAuthCallbackServerConfig,
    pub success_html: String,
    pub error_html: String,
    result_tx: Arc<Mutex<Option<CallbackSender>>>,
    result_rx: Option<CallbackReceiver>,
    resolved: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
    expected_state: Option<String>,
    paste_only: bool,
}

type CallbackResult = Result<BTreeMap<String, String>, OAuthError>;
type CallbackSender = oneshot::Sender<CallbackResult>;
type CallbackReceiver = oneshot::Receiver<CallbackResult>;

impl OAuthCallbackServer {
    pub const DEFAULT_SUCCESS_HTML: &'static str = r#"<!doctype html>
<html><head><meta charset=utf-8><title>kw - login complete</title>
<style>body{font-family:ui-sans-serif,system-ui;padding:3em;max-width:30em;margin:0 auto;color:#222}
h1{color:#0a7}</style></head>
<body><h1>Login complete</h1>
<p>You can close this tab and return to your terminal.</p></body></html>
"#;

    pub const DEFAULT_ERROR_HTML: &'static str = r#"<!doctype html>
<html><head><meta charset=utf-8><title>kw - login failed</title>
<style>body{font-family:ui-sans-serif,system-ui;padding:3em;max-width:30em;margin:0 auto;color:#222}
h1{color:#c33}</style></head>
<body><h1>Login failed</h1><p>{{message}}</p></body></html>
"#;

    pub fn new(port: u16) -> Self {
        Self::with_config(OAuthCallbackServerConfig::new(port))
    }

    pub fn with_path(port: u16, path: impl Into<String>) -> Self {
        Self::with_config(OAuthCallbackServerConfig::with_path(port, path))
    }

    pub fn with_redirect_host(
        port: u16,
        path: impl Into<String>,
        redirect_host: impl Into<String>,
    ) -> Self {
        Self::with_config(
            OAuthCallbackServerConfig::with_path(port, path).with_redirect_host(redirect_host),
        )
    }

    pub fn with_config(config: OAuthCallbackServerConfig) -> Self {
        let (result_tx, result_rx) = oneshot::channel();
        Self {
            config,
            success_html: Self::DEFAULT_SUCCESS_HTML.to_string(),
            error_html: Self::DEFAULT_ERROR_HTML.to_string(),
            result_tx: Arc::new(Mutex::new(Some(result_tx))),
            result_rx: Some(result_rx),
            resolved: Arc::new(AtomicBool::new(false)),
            closed: Arc::new(AtomicBool::new(false)),
            shutdown_tx: None,
            task: None,
            expected_state: None,
            paste_only: false,
        }
    }

    pub fn redirect_uri(&self) -> String {
        self.config.redirect_uri()
    }

    pub fn expect_state(&mut self, state: impl Into<String>) {
        self.expected_state = Some(state.into());
    }

    /// A fixed redirect must remain unchanged when another program owns its port.
    pub async fn start_with_paste(&mut self) -> Result<Option<&'static str>, OAuthError> {
        match self.start().await {
            Ok(()) => Ok(None),
            Err(error) if self.config.port != 0 && self.expected_state.is_some() => {
                self.paste_only = true;
                let reserved =
                    matches!(&error, OAuthError::Transport(message) if message.contains("10013"));
                Ok(Some(if reserved { "reserved" } else { "busy" }))
            }
            Err(error) => Err(error),
        }
    }

    pub fn callback_input(&self) -> Option<OAuthCallbackInput> {
        self.expected_state.as_ref()?;
        Some(OAuthCallbackInput {
            redirect: self.redirect_uri(),
            shared: Arc::new(self.shared()),
        })
    }

    fn shared(&self) -> CallbackShared {
        CallbackShared {
            path: self.config.path.clone(),
            expected_state: self.expected_state.clone(),
            success_html: self.success_html.clone(),
            error_html: self.error_html.clone(),
            result_tx: Arc::clone(&self.result_tx),
            resolved: Arc::clone(&self.resolved),
            closed: Arc::clone(&self.closed),
        }
    }

    /// Start listening on 127.0.0.1. Idempotent.
    pub async fn start(&mut self) -> Result<(), OAuthError> {
        if self.task.is_some() {
            return Ok(());
        }

        let listener = TcpListener::bind((self.config.host.as_str(), self.config.port))
            .await
            .map_err(|error| OAuthError::Transport(error.to_string()))?;
        self.config.port = listener
            .local_addr()
            .map_err(|error| OAuthError::Transport(error.to_string()))?
            .port();

        let shared = Arc::new(self.shared());
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        self.shutdown_tx = Some(shutdown_tx);
        self.task = Some(tokio::spawn(async move {
            accept_loop(listener, shared, shutdown_rx).await;
        }));

        Ok(())
    }

    /// Resolve with whichever completes first: a callback request or cancel().
    pub async fn wait_for_callback(&mut self) -> Result<BTreeMap<String, String>, OAuthError> {
        if !self.paste_only && !self.resolved.load(Ordering::SeqCst) {
            self.start().await?;
        }
        let receiver = self
            .result_rx
            .take()
            .ok_or_else(|| OAuthError::Transport("callback server already resolved".to_string()))?;

        let result = receiver
            .await
            .unwrap_or_else(|_| Err(OAuthError::Transport("callback server stopped".to_string())));
        if self.expected_state.is_none() {
            self.stop().await;
        }
        result
    }

    /// Stop listening; subsequent connections are rejected.
    pub async fn stop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }

    /// Unblock wait_for_callback() with OAuthError::Cancelled.
    pub fn cancel(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        complete(&self.result_tx, &self.resolved, Err(OAuthError::Cancelled));
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl Drop for OAuthCallbackServer {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// A local injection point for the exact same callback handler as the listener.
/// Pasted addresses are validated, never fetched.
#[derive(Clone)]
pub struct OAuthCallbackInput {
    redirect: String,
    shared: Arc<CallbackShared>,
}

impl OAuthCallbackInput {
    pub fn submit(&self, raw: &str) -> Result<(), OAuthError> {
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(OAuthError::Cancelled);
        }
        let invalid = || {
            OAuthError::InvalidResponse(
                "Paste the complete callback address from this sign-in.".into(),
            )
        };
        let raw = raw.trim();
        if raw.len() > 16 * 1024 || raw.chars().any(char::is_control) {
            return Err(invalid());
        }
        let url = url::Url::parse(raw).map_err(|_| invalid())?;
        let expected = url::Url::parse(&self.redirect).map_err(|_| invalid())?;
        if url.scheme() != "http"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.port_or_known_default() != expected.port_or_known_default()
            || !matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
            || url.path() != expected.path()
            || url.query().is_none()
        {
            return Err(invalid());
        }
        let response = callback_response(&self.shared, url.as_str());
        if let Some(result) = response.result {
            complete(&self.shared.result_tx, &self.shared.resolved, result);
        } else if response.status != 200 {
            return Err(invalid());
        }
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(OAuthError::Cancelled);
        }
        // A valid duplicate joins the exchange already claimed by the browser.
        Ok(())
    }
}

// MARK: - Request handling

struct CallbackShared {
    path: String,
    expected_state: Option<String>,
    success_html: String,
    error_html: String,
    result_tx: Arc<Mutex<Option<CallbackSender>>>,
    resolved: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
}

async fn accept_loop(
    listener: TcpListener,
    shared: Arc<CallbackShared>,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut shutdown_rx => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                while connections.try_join_next().is_some() {}
                if connections.len() >= 32 { continue; }
                let shared = Arc::clone(&shared);
                connections.spawn(async move {
                    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle_connection(stream, shared)).await;
                });
            }
        }
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    shared: Arc<CallbackShared>,
) -> Result<(), std::io::Error> {
    let mut data = Vec::new();
    let mut chunk = [0_u8; 1024];

    while data.len() < 16 * 1024 {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        data.extend_from_slice(&chunk[..read]);
        if data.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }

    let request = String::from_utf8_lossy(&data);
    let Some(request_line) = request.lines().next() else {
        write_response(&mut stream, 400, "<p>missing request line</p>", &[]).await?;
        return Ok(());
    };
    let mut parts = request_line.split_whitespace();
    let Some(method) = parts.next() else {
        write_response(&mut stream, 400, "<p>missing request method</p>", &[]).await?;
        return Ok(());
    };
    let Some(uri) = parts.next() else {
        write_response(&mut stream, 400, "<p>missing request line</p>", &[]).await?;
        return Ok(());
    };

    let request_headers = parse_request_headers(&request);
    let cors_headers = cors_response_headers(&request_headers);
    if method.eq_ignore_ascii_case("OPTIONS") {
        let status = if cors_headers.is_empty() { 403 } else { 204 };
        write_response(&mut stream, status, "", &cors_headers).await?;
        return Ok(());
    }
    if !method.eq_ignore_ascii_case("GET") {
        write_response(&mut stream, 405, "<p>method not allowed</p>", &cors_headers).await?;
        return Ok(());
    }

    let response = callback_response(&shared, uri);
    write_response(&mut stream, response.status, &response.html, &cors_headers).await?;
    if let Some(result) = response.result {
        complete(&shared.result_tx, &shared.resolved, result);
    }

    Ok(())
}

struct CallbackResponse {
    status: u16,
    html: String,
    result: Option<CallbackResult>,
}

fn callback_response(shared: &CallbackShared, uri: &str) -> CallbackResponse {
    let url = if uri.starts_with("http://") || uri.starts_with("https://") {
        url::Url::parse(uri)
    } else {
        url::Url::parse(&format!("http://localhost{uri}"))
    };
    let Ok(url) = url else {
        return CallbackResponse {
            status: 400,
            html: error_html(shared, "bad URL"),
            result: None,
        };
    };

    if url.path() != shared.path {
        return CallbackResponse {
            status: 404,
            html: error_html(shared, "unknown path"),
            result: None,
        };
    }

    let mut params = BTreeMap::new();
    for (key, value) in url.query_pairs() {
        if params
            .insert(key.into_owned(), value.into_owned())
            .is_some()
            && shared.expected_state.is_some()
        {
            return CallbackResponse {
                status: 400,
                html: error_html(shared, "Invalid callback parameters."),
                result: None,
            };
        }
    }
    if let Some(state) = &shared.expected_state {
        if params.get("state") != Some(state)
            || (!params.contains_key("error")
                && !params.get("code").is_some_and(|code| {
                    !code.is_empty() && code.len() <= 8192 && !code.chars().any(char::is_control)
                }))
        {
            return CallbackResponse {
                status: 400,
                html: error_html(shared, "This callback does not match the waiting sign-in."),
                result: None,
            };
        }
        if shared.resolved.load(Ordering::SeqCst) {
            return CallbackResponse {
                status: 200,
                html: "<p>This sign-in is already finishing. Return to Nora.</p>".into(),
                result: None,
            };
        }
    }

    if let Some(error) = params.get("error").cloned() {
        return CallbackResponse {
            status: 400,
            html: error_html(shared, &error),
            result: Some(Err(OAuthError::InvalidResponse(error))),
        };
    }

    CallbackResponse {
        status: 200,
        html: shared.success_html.clone(),
        result: Some(Ok(params)),
    }
}

fn error_html(shared: &CallbackShared, message: &str) -> String {
    let message = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    shared.error_html.replace("{{message}}", &message)
}

fn complete(
    result_tx: &Arc<Mutex<Option<CallbackSender>>>,
    resolved: &AtomicBool,
    result: CallbackResult,
) -> bool {
    if resolved.swap(true, Ordering::SeqCst) {
        return false;
    }
    if let Some(sender) = result_tx.lock().expect("callback result lock").take() {
        let _ = sender.send(result);
    }
    true
}

async fn write_response(
    stream: &mut TcpStream,
    status: u16,
    html: &str,
    headers: &[(String, String)],
) -> Result<(), std::io::Error> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let extra_headers = headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect::<String>();
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\n{extra_headers}connection: close\r\n\r\n{}",
        html.len(), html
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

fn parse_request_headers(request: &str) -> BTreeMap<String, String> {
    request
        .lines()
        .skip(1)
        .take_while(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect()
}

fn cors_response_headers(headers: &BTreeMap<String, String>) -> Vec<(String, String)> {
    let Some(origin) = headers
        .get("origin")
        .filter(|origin| origin.as_str() == XAI_ACCOUNTS_ORIGIN)
    else {
        return Vec::new();
    };
    let mut response = vec![
        ("Access-Control-Allow-Origin".to_string(), origin.clone()),
        (
            "Access-Control-Allow-Methods".to_string(),
            "GET, OPTIONS".to_string(),
        ),
        ("Vary".to_string(), "Origin".to_string()),
    ];
    if let Some(requested_headers) = headers.get("access-control-request-headers") {
        response.push((
            "Access-Control-Allow-Headers".to_string(),
            requested_headers.clone(),
        ));
    }
    if headers
        .get("access-control-request-private-network")
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    {
        response.push((
            "Access-Control-Allow-Private-Network".to_string(),
            "true".to_string(),
        ));
    }
    response
}
