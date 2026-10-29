use super::{
    network::{self, Client},
    protobuf::{self, Pb},
};
use futures::StreamExt;
use nora_ai::{HttpByteStream, HttpClientError, HttpStreamingResponse};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use std::{
    collections::BTreeMap,
    error::Error,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};
use tokio::sync::{mpsc, watch, Mutex, OwnedSemaphorePermit, Semaphore};
struct Packet {
    data: Vec<u8>,
    _permit: OwnedSemaphorePermit,
}
struct H1 {
    client: Client,
    token: String,
    version: String,
    id: String,
    sequence: u64,
}
enum Backend {
    H1(Mutex<H1>),
    H2(Mutex<Option<mpsc::Sender<Packet>>>, Arc<Semaphore>),
}
pub(crate) struct Writer {
    backend: Backend,
    error: watch::Sender<Option<PluginError>>,
    closed: watch::Sender<bool>,
    meter: nora_provider_plugin_sdk::RequestMeterContext,
}
impl Writer {
    pub async fn send(&self, message: Vec<u8>) -> PluginResult<()> {
        let mut closed = self.closed.subscribe();
        if *closed.borrow() {
            return Err(PluginError::transport("Cursor upload closed."));
        }
        tokio::select! {
            biased;
            _ = closed.changed() => Err(PluginError::transport("Cursor upload closed.")),
            result = self.meter.scope(self.send_inner(message)) => result,
        }
    }
    async fn send_inner(&self, message: Vec<u8>) -> PluginResult<()> {
        if message.len() > 16 * 1024 * 1024 {
            return Err(PluginError::transport(
                "Cursor outbound message limit reached.",
            ));
        }
        match &self.backend {
            Backend::H1(state) => {
                let mut state = state.lock().await;
                let hex = message
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>();
                let seq = state.sequence;
                state.sequence = state
                    .sequence
                    .checked_add(1)
                    .ok_or_else(protobuf::malformed)?;
                let body = Pb::new()
                    .str(1, &hex)
                    .bytes(2, Pb::new().str(1, &state.id))
                    .num(3, seq);
                let response = network::send(
                    state.client.as_ref(),
                    network::post(
                        format!("{}/aiserver.v1.BidiService/BidiAppend", network::API),
                        body.0,
                        &state.token,
                        &state.version,
                        "application/proto",
                    )
                    .with_header("x-request-id", &state.id)
                    .with_header("x-cursor-streaming", "true")
                    .with_header("accept-encoding", "identity"),
                    60 + (message.len() / 65536) as u64,
                )
                .await?;
                if !(200..300).contains(&response.status) {
                    return network::read(response).await.map(|_| ());
                }
                // Acknowledgment bodies are not prompts or user output.
                nora_ai::collect_http_body_text_limited(response.body, 1024 * 1024)
                    .await
                    .map_err(|_| {
                        PluginError::transport("Cursor did not acknowledge an uploaded message.")
                    })?;
                Ok(())
            }
            Backend::H2(sender, budget) => {
                let data = protobuf::frame(&message);
                let permit = tokio::time::timeout(
                    Duration::from_secs(30),
                    budget.clone().acquire_many_owned(data.len() as u32),
                )
                .await
                .map_err(|_| PluginError::transport("Cursor upload backpressure timed out."))?
                .map_err(|_| PluginError::transport("Cursor upload closed."))?;
                let sender = sender
                    .lock()
                    .await
                    .clone()
                    .ok_or_else(|| PluginError::transport("Cursor upload closed."))?;
                tokio::time::timeout(
                    Duration::from_secs(30),
                    sender.send(Packet {
                        data,
                        _permit: permit,
                    }),
                )
                .await
                .map_err(|_| PluginError::transport("Cursor upload backpressure timed out."))?
                .map_err(|_| PluginError::transport("Cursor upload closed."))
            }
        }
    }
    pub fn fail(&self, error: PluginError) {
        let _ = self.error.send(Some(error));
    }
    pub fn close(&self) {
        self.closed.send_replace(true);
        if let Backend::H2(sender, budget) = &self.backend {
            budget.close();
            if let Ok(mut sender) = sender.try_lock() {
                sender.take();
            }
        }
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        self.close();
    }
}
pub(crate) struct Transport {
    pub writer: Arc<Writer>,
    body: Option<HttpByteStream>,
    failed: watch::Receiver<Option<PluginError>>,
    heartbeat: tokio::task::JoinHandle<()>,
}
impl Drop for Transport {
    fn drop(&mut self) {
        self.close();
    }
}
impl Transport {
    pub fn close(&mut self) {
        self.heartbeat.abort();
        self.writer.close();
        self.body.take();
    }
    pub async fn next(&mut self) -> PluginResult<Option<Vec<u8>>> {
        let failed = self.failed.borrow().clone();
        if let Some(error) = failed {
            self.close();
            return Err(error);
        }
        let Some(body) = self.body.as_mut() else {
            return Ok(None);
        };
        let result = tokio::select! {
            changed=self.failed.changed()=>{let _=changed;Err(self.failed.borrow().clone().unwrap_or_else(||PluginError::transport("Cursor transport closed.")))},
            chunk=body.next()=>match chunk{Some(Ok(bytes))=>Ok(Some(bytes.to_vec())),Some(Err(_))=>Err(PluginError::transport("Cursor reply broke off before completion.")),None=>Ok(None)}
        };
        if !matches!(result, Ok(Some(_))) {
            self.close();
        }
        result
    }
}
fn heartbeat(writer: Arc<Writer>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(5));
        timer.tick().await;
        loop {
            timer.tick().await;
            if let Err(error) = writer.send(Pb::new().bytes(7, []).0).await {
                writer.fail(error);
                break;
            }
        }
    })
}
async fn wrap(
    response: HttpStreamingResponse,
    writer: Arc<Writer>,
    failed: watch::Receiver<Option<PluginError>>,
) -> PluginResult<Transport> {
    if !(200..300).contains(&response.status) {
        return match network::read(response).await {
            Err(error) => Err(error),
            Ok(_) => Err(PluginError::transport("Cursor rejected its stream.")),
        };
    }
    let heartbeat = heartbeat(writer.clone());
    Ok(Transport {
        writer,
        body: Some(response.body),
        failed,
        heartbeat,
    })
}
async fn h1(client: Client, token: &str, version: &str, first: Vec<u8>) -> PluginResult<Transport> {
    let id = super::request::uuid();
    let (error, failed) = watch::channel(None);
    let writer = Arc::new(Writer {
        backend: Backend::H1(Mutex::new(H1 {
            client: client.clone(),
            token: token.into(),
            version: version.into(),
            id: id.clone(),
            sequence: 0,
        })),
        error,
        closed: watch::channel(false).0,
        meter: nora_provider_plugin_sdk::RequestMeterContext::capture(),
    });
    let request = network::post(
        format!("{}/agent.v1.AgentService/RunSSE", network::API),
        protobuf::frame(&Pb::new().str(1, &id).0),
        token,
        version,
        "application/connect+proto",
    )
    .with_header("x-request-id", &id)
    .with_header("x-cursor-streaming", "true")
    .with_header("accept-encoding", "identity")
    .with_header(
        "x-cursor-agent-allowed-tools",
        "mcp_tool_call,get_mcp_tools_tool_call",
    );
    let response = network::send(client.as_ref(), request, 60).await?;
    let transport = wrap(response, writer, failed).await?;
    transport.writer.send(first).await?;
    Ok(transport)
}
async fn h2(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    team_id: Option<u64>,
    version: &str,
    first: Vec<u8>,
    head_wait: Duration,
) -> PluginResult<Transport> {
    let (tx, rx) = mpsc::channel::<Packet>(256);
    let (error, failed) = watch::channel(None);
    let budget = Arc::new(Semaphore::new(16 * 1024 * 1024 + 5));
    let writer = Arc::new(Writer {
        backend: Backend::H2(Mutex::new(Some(tx)), budget),
        error,
        closed: watch::channel(false).0,
        meter: nora_provider_plugin_sdk::RequestMeterContext::capture(),
    });
    writer.send(first).await?;
    let body = futures::stream::unfold(rx, |mut rx| async move {
        let packet = rx.recv().await?;
        Some((Ok::<_, std::io::Error>(packet.data), rx))
    });
    nora_provider_plugin_sdk::before_http_request().await?;
    let request = client
        .post(format!("{base}/agent.v1.AgentService/Run"))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/connect+proto")
        .header("connect-protocol-version", "1")
        .header("x-cursor-client-version", version)
        .header("x-cursor-client-type", "cli")
        .header("x-ghost-mode", "true")
        .header("x-request-id", super::request::uuid())
        .header(
            "x-cursor-agent-allowed-tools",
            "mcp_tool_call,get_mcp_tools_tool_call",
        )
        .body(reqwest::Body::wrap_stream(body));
    let request = if let Some(team) = team_id {
        request.header("x-cursor-team-id", team.to_string())
    } else {
        request
    };
    let response = tokio::time::timeout(head_wait, request.send())
        .await
        .map_err(|_| PluginError::new("cursor_h2_blocked", "Cursor HTTP/2 did not open in time."))?
        .map_err(|error| {
            PluginError::new(
                if h2_blocked(&error) {
                    "cursor_h2_blocked"
                } else {
                    "cursor_h2_unavailable"
                },
                "Cursor HTTP/2 could not open on this network.",
            )
        })?;
    if response.version() != reqwest::Version::HTTP_2 {
        return Err(PluginError::new(
            "cursor_h2_blocked",
            "Cursor requires HTTP/2 for this endpoint.",
        ));
    }
    let status = response.status().as_u16();
    let response = HttpStreamingResponse {
        status,
        headers: Default::default(),
        body: Box::pin(response.bytes_stream().map(|v| {
            v.map_err(|_| HttpClientError::Transport("Cursor stream interrupted.".into()))
        })),
    };
    wrap(response, writer, failed).await
}
fn h2_blocked(error: &reqwest::Error) -> bool {
    if error.is_connect() || error.is_timeout() {
        return false;
    }
    let mut source = error.source();
    while let Some(error) = source {
        if error.downcast_ref::<std::io::Error>().is_some() {
            return false;
        }
        if let Some(error) = error.downcast_ref::<h2::Error>() {
            if error.is_io() {
                return false;
            }
            return error.reason() == Some(h2::Reason::HTTP_1_1_REQUIRED)
                || !error.is_reset() && error.reason() == Some(h2::Reason::PROTOCOL_ERROR);
        }
        source = error.source();
    }
    false
}

pub(crate) struct Connector {
    blocked: StdMutex<BTreeMap<(String, String), tokio::time::Instant>>,
    head_wait: Duration,
    retry_after: Duration,
}
impl Default for Connector {
    fn default() -> Self {
        Self {
            blocked: StdMutex::new(BTreeMap::new()),
            head_wait: Duration::from_secs(15),
            retry_after: Duration::from_secs(10 * 60),
        }
    }
}
impl Connector {
    pub fn reset(&self, scope: &str) {
        self.blocked
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retain(|(network, _), _| network != scope);
    }
    fn blocked(&self, scope: &str, base: &str) -> bool {
        let mut entries = self
            .blocked
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        entries.retain(|_, until| *until > tokio::time::Instant::now());
        entries.contains_key(&(scope.into(), base.into()))
    }
    fn block(&self, scope: &str, base: &str) {
        let mut entries = self
            .blocked
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if entries.len() >= 64 {
            if let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, until)| **until)
                .map(|(key, _)| key.clone())
            {
                entries.remove(&oldest);
            }
        }
        entries.insert(
            (scope.into(), base.into()),
            tokio::time::Instant::now() + self.retry_after,
        );
    }
    pub(crate) async fn open(
        &self,
        scope: &str,
        client: Client,
        h2_client: Option<&reqwest::Client>,
        base: &str,
        token: &str,
        team_id: Option<u64>,
        version: &str,
        first: Vec<u8>,
    ) -> PluginResult<Transport> {
        if let Some(h2_client) = h2_client.filter(|_| !self.blocked(scope, base)) {
            match h2(
                h2_client,
                base,
                token,
                team_id,
                version,
                first.clone(),
                self.head_wait,
            )
            .await
            {
                Ok(t) => return Ok(t),
                Err(e) if e.code == "cursor_h2_unavailable" => {}
                Err(e) if e.code == "cursor_h2_blocked" => self.block(scope, base),
                Err(e) => return Err(e),
            }
        }
        h1(client, token, version, first).await
    }
}

#[cfg(test)]
#[path = "../test/transport.test.rs"]
mod tests;
