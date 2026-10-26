use super::*;
use crate::fixture::{Client as Script, Reply};
use async_trait::async_trait;
use bytes::Bytes;
use futures::future::poll_fn;
use nora_ai::{HttpClient, HttpRequest};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex as StdMutex;
use tokio::net::TcpListener;
use tokio::sync::Notify;

#[derive(Clone, Copy)]
enum Mode {
    Answer(u16),
    Hold,
    Reset,
}

struct Server {
    url: String,
    requests: mpsc::UnboundedReceiver<http::request::Parts>,
    data: mpsc::UnboundedReceiver<Vec<u8>>,
    resets: mpsc::UnboundedReceiver<()>,
    task: tokio::task::JoinHandle<()>,
    mode: Arc<StdMutex<Mode>>,
}

struct StreamClosed(mpsc::UnboundedSender<()>);
impl Drop for StreamClosed {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn server(mode: Mode) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (requests_tx, requests) = mpsc::unbounded_channel();
    let (data_tx, data) = mpsc::unbounded_channel();
    let (resets_tx, resets) = mpsc::unbounded_channel();
    let mode = Arc::new(StdMutex::new(mode));
    let behavior = mode.clone();
    let task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let requests_tx = requests_tx.clone();
            let data_tx = data_tx.clone();
            let resets_tx = resets_tx.clone();
            let behavior = behavior.clone();
            connections.spawn(async move {
                let Ok(mut connection) = h2::server::handshake(socket).await else {
                    return;
                };
                let mut streams = tokio::task::JoinSet::new();
                while let Some(Ok((request, mut respond))) = connection.accept().await {
                    let (parts, mut body) = request.into_parts();
                    let _ = requests_tx.send(parts);
                    let data_tx = data_tx.clone();
                    let resets_tx = resets_tx.clone();
                    let mode = *behavior.lock().unwrap();
                    streams.spawn(async move {
                        let _closed = StreamClosed(resets_tx.clone());
                        if matches!(mode, Mode::Reset) {
                            respond.send_reset(h2::Reason::REFUSED_STREAM);
                            return;
                        }
                        if matches!(mode, Mode::Hold) {
                            let _ = poll_fn(|cx| respond.poll_reset(cx)).await;
                            let _ = resets_tx.send(());
                            return;
                        }
                        let Mode::Answer(status) = mode else {
                            unreachable!()
                        };
                        let mut send = respond
                            .send_response(
                                http::Response::builder().status(status).body(()).unwrap(),
                                false,
                            )
                            .unwrap();
                        let bytes = if status == 200 {
                            protobuf::frame(b"answer")
                        } else {
                            json!({"code":"unauthenticated","message":"private upstream detail"})
                                .to_string()
                                .into_bytes()
                        };
                        send.send_data(Bytes::from(bytes), status != 200).unwrap();
                        let incoming = async {
                            while let Some(Ok(bytes)) = body.data().await {
                                body.flow_control().release_capacity(bytes.len()).unwrap();
                                let _ = data_tx.send(bytes.to_vec());
                            }
                        };
                        let reset = async {
                            let _ = poll_fn(|cx| send.poll_reset(cx)).await;
                            let _ = resets_tx.send(());
                        };
                        tokio::join!(incoming, reset);
                    });
                }
            });
        }
    });
    Server {
        url,
        requests,
        data,
        resets,
        task,
        mode,
    }
}

async fn open(
    client: Client,
    h2_client: Option<&reqwest::Client>,
    base: &str,
    token: &str,
    version: &str,
    first: Vec<u8>,
) -> PluginResult<Transport> {
    Connector::default()
        .open(
            "fixture-network",
            client,
            h2_client,
            base,
            token,
            None,
            version,
            first,
        )
        .await
}

#[tokio::test]
async fn a_missing_h2_head_temporarily_uses_h1_only_for_its_network_and_host_then_retries() {
    let connector = Connector {
        head_wait: Duration::from_millis(100),
        retry_after: Duration::from_millis(250),
        ..Default::default()
    };
    let mut held = server(Mode::Hold).await;
    let fallback = Script::new((0..4).map(|_| Reply::json(json!({}))).collect());
    let client = h2_client();
    drop(
        connector
            .open(
                "proxy-a",
                fallback.clone(),
                Some(&client),
                &held.url,
                "token",
                None,
                "cli-fixture",
                b"first".to_vec(),
            )
            .await
            .unwrap(),
    );
    received(&mut held.requests).await;
    assert!(connector.blocked("proxy-a", &held.url));
    *held.mode.lock().unwrap() = Mode::Answer(200);
    drop(
        connector
            .open(
                "proxy-a",
                fallback.clone(),
                Some(&client),
                &held.url,
                "token",
                None,
                "cli-fixture",
                b"cached".to_vec(),
            )
            .await
            .unwrap(),
    );
    assert!(held.requests.try_recv().is_err());
    drop(
        connector
            .open(
                "proxy-b",
                fallback.clone(),
                Some(&client),
                &held.url,
                "token",
                None,
                "cli-fixture",
                b"other proxy".to_vec(),
            )
            .await
            .unwrap(),
    );
    received(&mut held.requests).await;
    let mut other_host = server(Mode::Answer(200)).await;
    drop(
        connector
            .open(
                "proxy-a",
                fallback.clone(),
                Some(&client),
                &other_host.url,
                "token",
                None,
                "cli-fixture",
                b"other host".to_vec(),
            )
            .await
            .unwrap(),
    );
    received(&mut other_host.requests).await;
    tokio::time::sleep(Duration::from_millis(260)).await;
    drop(
        connector
            .open(
                "proxy-a",
                fallback.clone(),
                Some(&client),
                &held.url,
                "token",
                None,
                "cli-fixture",
                b"retry".to_vec(),
            )
            .await
            .unwrap(),
    );
    received(&mut held.requests).await;
    assert!(!connector.blocked("proxy-a", &held.url));
    assert_eq!(fallback.requests.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn connection_refusal_and_stream_refusal_never_mark_the_network_as_h2_blocked() {
    let connector = Connector::default();
    let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let refused = format!("http://{}", socket.local_addr().unwrap());
    drop(socket);
    let reset = server(Mode::Reset).await;
    let fallback = Script::new((0..4).map(|_| Reply::json(json!({}))).collect());
    let client = h2_client();
    for base in [&refused, &reset.url] {
        drop(
            connector
                .open(
                    "network",
                    fallback.clone(),
                    Some(&client),
                    base,
                    "token",
                    None,
                    "cli-fixture",
                    b"request".to_vec(),
                )
                .await
                .unwrap(),
        );
        assert!(!connector.blocked("network", base));
    }
}

#[tokio::test]
async fn cancellation_during_an_h2_open_does_not_poison_the_next_attempt() {
    let connector = Arc::new(Connector::default());
    let mut held = server(Mode::Hold).await;
    let fallback = Script::new(vec![]);
    let shared = connector.clone();
    let scripted = fallback.clone();
    let base = held.url.clone();
    let pending = tokio::spawn(async move {
        shared
            .open(
                "network",
                scripted,
                Some(&h2_client()),
                &base,
                "token",
                None,
                "cli-fixture",
                b"cancelled".to_vec(),
            )
            .await
    });
    received(&mut held.requests).await;
    pending.abort();
    assert!(matches!(pending.await,Err(error) if error.is_cancelled()));
    assert!(!connector.blocked("network", &held.url));
    *held.mode.lock().unwrap() = Mode::Answer(200);
    drop(
        connector
            .open(
                "network",
                fallback.clone(),
                Some(&h2_client()),
                &held.url,
                "token",
                None,
                "cli-fixture",
                b"next".to_vec(),
            )
            .await
            .unwrap(),
    );
    received(&mut held.requests).await;
    assert!(fallback.requests.lock().unwrap().is_empty());
}

#[test]
fn regional_retry_clears_only_its_network_scope_and_the_h2_cache_stays_bounded() {
    let connector = Connector::default();
    for index in 0..80 {
        connector.block("other", &format!("host-{index}"));
    }
    assert_eq!(connector.blocked.lock().unwrap().len(), 64);
    connector.block("region", "region-host");
    connector.reset("region");
    assert!(!connector.blocked("region", "region-host"));
    assert!(connector.blocked("other", "host-79"));
}

fn h2_client() -> reqwest::Client {
    reqwest::Client::builder()
        .http2_prior_knowledge()
        .no_proxy()
        .build()
        .unwrap()
}

async fn received<T>(rx: &mut mpsc::UnboundedReceiver<T>) -> T {
    tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn real_http2_sends_connect_frames_and_followups_without_opening_runsse() {
    let mut server = server(Mode::Answer(200)).await;
    let fallback = Script::new(vec![]);
    let mut transport = open(
        fallback.clone(),
        Some(&h2_client()),
        &server.url,
        "fixture-token",
        "cli-fixture",
        b"first".to_vec(),
    )
    .await
    .unwrap();
    let request = received(&mut server.requests).await;
    assert_eq!(request.uri.path(), "/agent.v1.AgentService/Run");
    assert_eq!(request.headers["authorization"], "Bearer fixture-token");
    assert_eq!(request.headers["x-ghost-mode"], "true");
    assert_eq!(
        request.headers["x-cursor-agent-allowed-tools"],
        "mcp_tool_call,get_mcp_tools_tool_call"
    );
    assert_eq!(received(&mut server.data).await, protobuf::frame(b"first"));
    transport.writer.send(b"followup".to_vec()).await.unwrap();
    assert_eq!(
        received(&mut server.data).await,
        protobuf::frame(b"followup")
    );
    assert_eq!(
        transport.next().await.unwrap().unwrap(),
        protobuf::frame(b"answer")
    );
    transport.close();
    received(&mut server.resets).await;
    assert!(fallback.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_refused_h2_stream_falls_back_once_and_the_next_request_can_use_h2_again() {
    let failing = server(Mode::Reset).await;
    let fallback = Script::new(vec![Reply::json(json!({})), Reply::json(json!({}))]);
    let first = b"one-run".to_vec();
    let transport = open(
        fallback.clone(),
        Some(&h2_client()),
        &failing.url,
        "token",
        "cli-fixture",
        first.clone(),
    )
    .await
    .unwrap();
    drop(transport);
    let requests = fallback.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].url.ends_with("/RunSSE"));
    assert!(requests[1].url.ends_with("/BidiAppend"));
    let fields = protobuf::fields(requests[1].body.as_ref().unwrap()).unwrap();
    assert_eq!(protobuf::num(&fields, 3).unwrap(), 0);
    assert_eq!(
        protobuf::string(&fields, 1).unwrap(),
        first
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    drop(requests);
    let working = server(Mode::Answer(200)).await;
    let second = open(
        fallback.clone(),
        Some(&h2_client()),
        &working.url,
        "token",
        "cli-fixture",
        b"next".to_vec(),
    )
    .await
    .unwrap();
    drop(second);
    assert_eq!(fallback.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn h2_account_refusals_preserve_status_without_replaying_the_prompt_over_h1() {
    let server = server(Mode::Answer(401)).await;
    let fallback = Script::new(vec![]);
    let error = open(
        fallback.clone(),
        Some(&h2_client()),
        &server.url,
        "token",
        "cli-fixture",
        b"private prompt".to_vec(),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.code, "authentication_required");
    assert!(!error.message.contains("private upstream detail"));
    assert!(fallback.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn dropping_an_h2_open_before_response_headers_cancels_it_without_fallback() {
    let mut server = server(Mode::Hold).await;
    let fallback = Script::new(vec![]);
    let client = fallback.clone();
    let url = server.url.clone();
    let pending = tokio::spawn(async move {
        open(
            client,
            Some(&h2_client()),
            &url,
            "token",
            "cli-fixture",
            b"first".to_vec(),
        )
        .await
    });
    received(&mut server.requests).await;
    pending.abort();
    assert!(matches!(pending.await, Err(error) if error.is_cancelled()));
    received(&mut server.resets).await;
    assert!(fallback.requests.lock().unwrap().is_empty());
}

struct H1Probe {
    requests: StdMutex<Vec<HttpRequest>>,
    run: StdMutex<Option<mpsc::UnboundedReceiver<Result<Bytes, HttpClientError>>>>,
    appends: AtomicUsize,
    hold_append: bool,
    append_entered: Notify,
    append_dropped: Arc<AtomicUsize>,
}

struct PendingAppend(Arc<AtomicUsize>);
impl Drop for PendingAppend {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn h1_probe(
    hold_append: bool,
) -> (
    Arc<H1Probe>,
    mpsc::UnboundedSender<Result<Bytes, HttpClientError>>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    (
        Arc::new(H1Probe {
            requests: StdMutex::new(vec![]),
            run: StdMutex::new(Some(rx)),
            appends: AtomicUsize::new(0),
            hold_append,
            append_entered: Notify::new(),
            append_dropped: Arc::new(AtomicUsize::new(0)),
        }),
        tx,
    )
}

#[async_trait]
impl HttpClient for H1Probe {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        let is_run = request.url.ends_with("/RunSSE");
        self.requests.lock().unwrap().push(request);
        if is_run {
            let rx = self.run.lock().unwrap().take().unwrap();
            return Ok(HttpStreamingResponse {
                status: 200,
                headers: Default::default(),
                body: Box::pin(futures::stream::unfold(rx, |mut rx| async move {
                    rx.recv().await.map(|value| (value, rx))
                })),
            });
        }
        let index = self.appends.fetch_add(1, Ordering::SeqCst);
        if self.hold_append && index == 1 {
            let _drop = PendingAppend(self.append_dropped.clone());
            self.append_entered.notify_one();
            futures::future::pending::<()>().await;
        }
        Ok(HttpStreamingResponse {
            status: 200,
            headers: Default::default(),
            body: Box::pin(futures::stream::empty()),
        })
    }
}

#[tokio::test]
async fn closing_runsse_cancels_the_pending_append_and_never_submits_queued_messages() {
    let (client, upstream) = h1_probe(true);
    let mut transport = open(
        client.clone(),
        None,
        "unused",
        "token",
        "cli-fixture",
        b"first".to_vec(),
    )
    .await
    .unwrap();
    let writer = transport.writer.clone();
    let pending = tokio::spawn(async move { writer.send(b"pending".to_vec()).await });
    client.append_entered.notified().await;
    let writer = transport.writer.clone();
    let queued = tokio::spawn(async move { writer.send(b"queued".to_vec()).await });
    tokio::task::yield_now().await;
    transport.close();
    assert!(tokio::time::timeout(Duration::from_millis(100), pending)
        .await
        .unwrap()
        .unwrap()
        .is_err());
    assert!(tokio::time::timeout(Duration::from_millis(100), queued)
        .await
        .unwrap()
        .unwrap()
        .is_err());
    assert_eq!(client.appends.load(Ordering::SeqCst), 2);
    assert_eq!(client.append_dropped.load(Ordering::SeqCst), 1);
    assert!(upstream.is_closed());
    assert!(transport
        .writer
        .send(b"after close".to_vec())
        .await
        .is_err());
}

#[tokio::test]
async fn a_runsse_read_failure_releases_the_body_before_returning_the_error() {
    let (client, upstream) = h1_probe(false);
    let mut transport = open(
        client,
        None,
        "unused",
        "token",
        "cli-fixture",
        b"first".to_vec(),
    )
    .await
    .unwrap();
    upstream
        .send(Err(HttpClientError::Transport("fixture".into())))
        .unwrap();
    assert!(transport.next().await.is_err());
    assert!(upstream.is_closed());
    assert!(transport
        .writer
        .send(b"after failure".to_vec())
        .await
        .is_err());
}

#[tokio::test]
async fn h1_error_statuses_survive_non_json_response_bodies() {
    for (on_append, status, expected) in [
        (false, 401, "authentication_required"),
        (true, 429, "cursor_http_429"),
    ] {
        let refused = Reply {
            status,
            chunks: vec![b"<html>refused</html>".to_vec()],
        };
        let replies = if on_append {
            vec![Reply::json(json!({})), refused]
        } else {
            vec![refused]
        };
        let client = Script::new(replies);
        let error = open(
            client.clone(),
            None,
            "unused",
            "token",
            "cli-fixture",
            b"first".to_vec(),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.code, expected);
        assert_eq!(
            client.requests.lock().unwrap().len(),
            if on_append { 2 } else { 1 }
        );
    }
}

#[tokio::test]
async fn native_http2_and_fallback_runs_use_the_selected_team() {
    let token = "e30.eyJzdWIiOiJhdXRoMHx1c2VyX29uZSJ9.sig";
    for team in [Some(42), None] {
        let mut server = server(Mode::Answer(200)).await;
        let fallback = Script::new(vec![]);
        let native = Connector::default()
            .open(
                "team-test",
                fallback.clone(),
                Some(&h2_client()),
                &server.url,
                token,
                team,
                "cli-test",
                b"first".to_vec(),
            )
            .await
            .unwrap();
        let request = received(&mut server.requests).await;
        assert_eq!(
            request
                .headers
                .get("x-cursor-team-id")
                .map(|value| value.to_str().unwrap().to_owned()),
            team.map(|value| value.to_string())
        );
        assert!(fallback.requests.lock().unwrap().is_empty());
        drop(native);
        let fallback = Script::new(vec![Reply::json(json!({})), Reply::json(json!({}))]);
        let scoped = crate::team::fixed_client(fallback.clone(), token, team);
        let run = Connector::default()
            .open(
                "team-test",
                scoped,
                None,
                "https://agentn.global.api5.cursor.sh",
                token,
                team,
                "cli-test",
                b"first".to_vec(),
            )
            .await
            .unwrap();
        let requests = fallback.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].url.ends_with("/RunSSE"));
        assert!(requests[1].url.ends_with("/BidiAppend"));
        for request in requests.iter() {
            assert_eq!(
                request.headers.get("x-cursor-team-id").cloned(),
                team.map(|value| value.to_string())
            );
        }
        drop(run);
    }
}
