use super::*;
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use nora_ai::ReqwestHttpClient;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::{JoinHandle, JoinSet},
};
use tokio_tungstenite::{
    tungstenite::{
        handshake::derive_accept_key,
        protocol::{frame::coding::CloseCode, CloseFrame, Role},
    },
    WebSocketStream,
};

#[derive(Clone, Default)]
struct Reply {
    refusal: Option<u16>,
    bad_accept: bool,
    frames: Vec<Message>,
    close: Option<CloseCode>,
    hold: bool,
}

#[derive(Default)]
struct Seen {
    method: String,
    target: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

struct Backend {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    closed: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl Backend {
    async fn new(reply: Reply) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/backend-api/codex/responses",
            listener.local_addr().unwrap()
        );
        let seen = Arc::new(Mutex::new(Vec::new()));
        let requests = seen.clone();
        let closed = Arc::new(AtomicUsize::new(0));
        let connections_closed = closed.clone();
        let task = tokio::spawn(async move {
            let mut tasks = JoinSet::new();
            loop {
                tokio::select! {
                    connection = listener.accept() => {
                        let (stream, _) = connection.unwrap();
                        let reply = reply.clone();
                        let requests = requests.clone();
                        let closed = connections_closed.clone();
                        tasks.spawn(async move { serve(stream, reply, requests, closed).await });
                    }
                    Some(result) = tasks.join_next(), if !tasks.is_empty() => result.unwrap(),
                }
            }
        });
        Self {
            url,
            seen,
            closed,
            task,
        }
    }

    fn counts(&self) -> (usize, usize) {
        let seen = self.seen.lock().unwrap();
        (
            seen.iter()
                .filter(|request| request.method == "GET")
                .count(),
            seen.iter()
                .filter(|request| request.method == "POST")
                .count(),
        )
    }

    async fn wait_closed(&self) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while self.closed.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("The cancelled WebSocket must close its connection");
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(
    mut stream: TcpStream,
    reply: Reply,
    requests: Arc<Mutex<Vec<Seen>>>,
    closed: Arc<AtomicUsize>,
) {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let byte = stream.read_u8().await.unwrap();
        bytes.push(byte);
    }
    let text = String::from_utf8(bytes).unwrap();
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap().split_whitespace();
    let method = first.next().unwrap().to_owned();
    let target = first.next().unwrap().to_owned();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect::<BTreeMap<_, _>>();
    let mut body = vec![
        0;
        headers
            .get("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    ];
    stream.read_exact(&mut body).await.unwrap();
    let index = {
        let mut requests = requests.lock().unwrap();
        let index = requests.len();
        requests.push(Seen {
            method: method.clone(),
            target,
            headers: headers.clone(),
            body,
        });
        index
    };
    if method == "POST" {
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nHTTP fallback")
            .await
            .unwrap();
        return;
    }
    if let Some(status) = reply.refusal {
        stream
            .write_all(
                format!(
                    "HTTP/1.1 {status} Refused\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        return;
    }
    let accept = if reply.bad_accept {
        "invalid-accept-proof".to_owned()
    } else {
        derive_accept_key(headers.get("sec-websocket-key").unwrap().as_bytes())
    };
    stream
        .write_all(format!("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {accept}\r\nX-Codex-Plan-Type: plus\r\n\r\n").as_bytes())
        .await
        .unwrap();
    if reply.bad_accept {
        return;
    }
    let mut socket = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
    let message = socket.next().await.unwrap().unwrap();
    requests.lock().unwrap()[index].body = message.into_data();
    for frame in reply.frames {
        if socket.send(frame).await.is_err() {
            return;
        }
    }
    if reply.hold {
        while socket.next().await.is_some_and(|message| message.is_ok()) {}
        closed.fetch_add(1, Ordering::SeqCst);
        return;
    }
    let code = reply.close.unwrap_or(CloseCode::Normal);
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: "".into(),
        })))
        .await;
}

fn request(url: &str) -> HttpRequest {
    HttpRequest::new(HttpMethod::Post, url)
        .with_header("authorization", "Bearer saved-account-token")
        .with_header("chatgpt-account-id", "saved-account")
        .with_header("originator", "codex_cli_rs")
        .with_header("user-agent", "codex_cli_rs/0.159.0")
        .with_header("openai-beta", "responses=experimental")
        .with_header("content-type", "application/json")
        .with_header("accept", "text/event-stream")
        .with_json_body(&json!({"model":"gpt-5.5","stream":true,"input":"ping","client_metadata":{"session_id":"session"}}))
        .unwrap()
}

fn client(proxy: Option<&NetworkProxy>) -> (ReqwestHttpClient, Arc<AtomicUsize>) {
    let attempts = Arc::new(AtomicUsize::new(0));
    let observed = attempts.clone();
    let client =
        ReqwestHttpClient::with_client(nora_provider_plugin_sdk::http_client(proxy).unwrap())
            .with_observer_fn(move || {
                observed.fetch_add(1, Ordering::SeqCst);
                async { Ok(()) }
            });
    (client, attempts)
}

fn text(value: Value) -> Message {
    Message::Text(value.to_string())
}

fn complete() -> Vec<Message> {
    vec![
        text(
            json!({"type":"codex.rate_limits","plan_type":"pro","rate_limits":{"primary":{"used_percent":42,"window_minutes":300,"reset_at":1792000000},"secondary":{"used_percent":7.5}},"credits":{"has_credits":false,"unlimited":false,"balance":"0"}}),
        ),
        text(
            json!({"type":"codex.response.metadata","headers":{"x-codex-turn-state":"state-1","x-models-etag":"models"}}),
        ),
        text(json!({"type":"response.created","response":{"id":"response"}})),
        text(json!({"type":"responsesapi.websocket_timing","timing":{"ms":3}})),
        Message::Text("{\"type\":\"response.output_text.delta\",\n\"delta\":\"pong\"}".into()),
        text(
            json!({"type":"response.completed","response":{"id":"response","usage":{"input_tokens":7,"output_tokens":1}}}),
        ),
    ]
}

async fn body(response: HttpStreamingResponse) -> String {
    nora_ai::collect_http_body_text_limited(response.body, 1024 * 1024)
        .await
        .unwrap()
}

#[tokio::test]
async fn final_compressed_turn_is_sent_as_websocket_and_returns_complete_sse() {
    let backend = Backend::new(Reply {
        frames: complete(),
        ..Default::default()
    })
    .await;
    let (client, posts) = client(Some(&NetworkProxy::Direct));
    let mut request = request(&backend.url)
        .with_header("x-codex-turn-state", "state-0")
        .with_header("x-openai-internal-codex-responses-lite", "true")
        .with_header("content-encoding", "zstd");
    request.body = Some(zstd::stream::encode_all(request.body.as_deref().unwrap(), 1).unwrap());
    let response = send_turn(request, &client, Some(&NetworkProxy::Direct))
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.headers["content-type"], "text/event-stream");
    assert_eq!(response.headers["x-codex-plan-type"], "plus");
    assert_eq!(response.headers["x-codex-primary-used-percent"], "42");
    assert_eq!(response.headers["x-codex-secondary-used-percent"], "7.5");
    assert_eq!(response.headers["x-codex-credits-has-credits"], "False");
    assert_eq!(response.headers["x-codex-turn-state"], "state-1");
    assert!(!response.headers.contains_key("upgrade"));
    assert!(!response.headers.contains_key("sec-websocket-accept"));
    let body = body(response).await;
    assert!(body.contains("event: response.output_text.delta\ndata: "));
    assert!(body.contains("pong"));
    assert!(body.contains("event: response.completed\ndata: "));
    assert!(!body.contains("websocket_timing"));
    assert_eq!(backend.counts(), (1, 0));
    assert_eq!(posts.load(Ordering::SeqCst), 0);
    let seen = backend.seen.lock().unwrap();
    let handshake = &seen[0];
    assert_eq!(
        handshake.headers["authorization"],
        "Bearer saved-account-token"
    );
    assert_eq!(handshake.headers["chatgpt-account-id"], "saved-account");
    assert_eq!(handshake.headers["openai-beta"], protocol::BETA);
    assert_eq!(
        handshake.headers.get("accept").map(String::as_str),
        Some("*/*")
    );
    for name in [
        "content-type",
        "content-encoding",
        "x-codex-turn-state",
        "x-openai-internal-codex-responses-lite",
    ] {
        assert!(!handshake.headers.contains_key(name), "{name}");
    }
    let message: Value = serde_json::from_slice(&handshake.body).unwrap();
    assert!(handshake
        .body
        .starts_with(b"{\"type\":\"response.create\","));
    assert_eq!(message["stream"], true);
    assert_eq!(message["model"], "gpt-5.5");
    assert_eq!(message["input"], "ping");
    assert_eq!(message["client_metadata"]["session_id"], "session");
    assert_eq!(message["client_metadata"]["x-codex-turn-state"], "state-0");
    assert_eq!(
        message["client_metadata"]["ws_request_header_x_openai_internal_codex_responses_lite"],
        "true"
    );
}

#[tokio::test]
async fn refused_handshake_reuses_exact_http_request_and_cools_down_the_host() {
    let backend = Backend::new(Reply {
        refusal: Some(403),
        ..Default::default()
    })
    .await;
    let (client, posts) = client(Some(&NetworkProxy::Direct));
    let mut original = request(&backend.url).with_header("content-encoding", "zstd");
    original.body = Some(zstd::stream::encode_all(original.body.as_deref().unwrap(), 1).unwrap());
    for _ in 0..2 {
        assert_eq!(
            body(
                send_turn(original.clone(), &client, Some(&NetworkProxy::Direct))
                    .await
                    .unwrap()
            )
            .await,
            "HTTP fallback"
        );
    }
    assert_eq!(backend.counts(), (1, 2));
    assert_eq!(posts.load(Ordering::SeqCst), 2);
    let seen = backend.seen.lock().unwrap();
    for post in seen.iter().filter(|request| request.method == "POST") {
        assert_eq!(post.body, *original.body.as_ref().unwrap());
        assert_eq!(post.headers["content-encoding"], "zstd");
        assert_eq!(post.headers["authorization"], "Bearer saved-account-token");
        assert_eq!(post.headers["openai-beta"], "responses=experimental");
    }
}

#[tokio::test]
async fn account_errors_do_not_disable_websockets_for_other_accounts() {
    for status in [401, 429, 500, 503] {
        let backend = Backend::new(Reply {
            refusal: Some(status),
            ..Default::default()
        })
        .await;
        let (client, posts) = client(Some(&NetworkProxy::Direct));
        for _ in 0..2 {
            body(
                send_turn(request(&backend.url), &client, Some(&NetworkProxy::Direct))
                    .await
                    .unwrap(),
            )
            .await;
        }
        assert_eq!(backend.counts(), (2, 2), "status {status}");
        assert_eq!(posts.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn incorrect_handshake_accept_is_rejected_and_falls_back_to_http() {
    let backend = Backend::new(Reply {
        bad_accept: true,
        ..Default::default()
    })
    .await;
    let (client, posts) = client(Some(&NetworkProxy::Direct));
    assert_eq!(
        body(
            send_turn(request(&backend.url), &client, Some(&NetworkProxy::Direct))
                .await
                .unwrap()
        )
        .await,
        "HTTP fallback"
    );
    assert_eq!(backend.counts(), (1, 1));
    assert_eq!(posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn socket_error_status_and_headers_are_returned_without_replaying_the_turn() {
    let backend = Backend::new(Reply {
        frames: vec![text(json!({"type":"error","status":429,"error":{"type":"usage_limit_reached","message":"No credits","headers":{"retry-after":"60"}},"headers":{"x-codex-primary-used-percent":"100"}}))],
        ..Default::default()
    }).await;
    let (client, posts) = client(Some(&NetworkProxy::Direct));
    let response = send_turn(request(&backend.url), &client, Some(&NetworkProxy::Direct))
        .await
        .unwrap();
    assert_eq!(response.status, 429);
    assert_eq!(response.headers["content-type"], "application/json");
    assert_eq!(response.headers["x-codex-primary-used-percent"], "100");
    assert_eq!(response.headers["retry-after"], "60");
    assert_eq!(
        serde_json::from_str::<Value>(&body(response).await).unwrap()["error"]["type"],
        "usage_limit_reached"
    );
    assert_eq!(backend.counts(), (1, 0));
    assert_eq!(posts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn connection_limit_and_close_before_any_response_fall_back_to_http() {
    let frames = vec![
        Vec::new(),
        vec![text(
            json!({"type":"error","error":{"code":"websocket_connection_limit_reached"}}),
        )],
        vec![text(
            json!({"type":"codex.response.metadata","headers":{"x-codex-turn-state":"state"}}),
        )],
    ];
    for frames in frames {
        let backend = Backend::new(Reply {
            frames,
            ..Default::default()
        })
        .await;
        let (client, posts) = client(Some(&NetworkProxy::Direct));
        assert_eq!(
            body(
                send_turn(request(&backend.url), &client, Some(&NetworkProxy::Direct))
                    .await
                    .unwrap()
            )
            .await,
            "HTTP fallback"
        );
        assert_eq!(backend.counts(), (1, 1));
        assert_eq!(posts.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn too_big_close_remembers_the_smallest_refused_message() {
    let backend = Backend::new(Reply {
        close: Some(CloseCode::Size),
        ..Default::default()
    })
    .await;
    let (client, posts) = client(Some(&NetworkProxy::Direct));
    let mut large = request(&backend.url);
    large.body = Some(
        json!({"stream":true,"input":"x".repeat(5000)})
            .to_string()
            .into_bytes(),
    );
    body(
        send_turn(large.clone(), &client, Some(&NetworkProxy::Direct))
            .await
            .unwrap(),
    )
    .await;
    body(
        send_turn(large, &client, Some(&NetworkProxy::Direct))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(backend.counts(), (1, 2));
    body(
        send_turn(request(&backend.url), &client, Some(&NetworkProxy::Direct))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(backend.counts(), (2, 3));
    assert_eq!(posts.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn a_response_that_has_started_is_never_replayed_after_the_socket_breaks() {
    let backend = Backend::new(Reply {
        frames: vec![text(
            json!({"type":"response.created","response":{"id":"began"}}),
        )],
        ..Default::default()
    })
    .await;
    let (client, posts) = client(Some(&NetworkProxy::Direct));
    let mut response = send_turn(request(&backend.url), &client, Some(&NetworkProxy::Direct))
        .await
        .unwrap();
    let first = response.body.next().await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&first).contains("response.created"));
    assert!(response.body.next().await.unwrap().is_err());
    assert!(response.body.next().await.is_none());
    assert_eq!(backend.counts(), (1, 0));
    assert_eq!(posts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn explicit_proxy_carries_the_upgrade_and_its_http_fallback() {
    for refusal in [None, Some(403)] {
        let proxy = Backend::new(Reply {
            refusal,
            frames: complete(),
            ..Default::default()
        })
        .await;
        let proxy_url = url::Url::parse(&proxy.url)
            .unwrap()
            .origin()
            .ascii_serialization();
        let proxy_config = NetworkProxy::Proxy { url: proxy_url };
        let (client, posts) = client(Some(&proxy_config));
        let request = request("http://vendor-for-proxy-test.invalid/backend-api/codex/responses");
        let response = websocket(
            &request,
            &protocol::message(&request).unwrap(),
            &proxy.url,
            Some(&proxy_config),
        )
        .await
        .unwrap();
        let response = match response {
            Some(response) => response,
            None => http(request, &client).await.unwrap(),
        };
        let body = body(response).await;
        if refusal.is_some() {
            assert_eq!(body, "HTTP fallback");
            assert_eq!(proxy.counts(), (1, 1));
            assert_eq!(posts.load(Ordering::SeqCst), 1);
        } else {
            assert!(body.contains("response.completed"));
            assert_eq!(proxy.counts(), (1, 0));
            assert_eq!(posts.load(Ordering::SeqCst), 0);
        }
        assert!(proxy.seen.lock().unwrap().iter().all(|request| request
            .target
            .starts_with("http://vendor-for-proxy-test.invalid/")));
    }
}

#[tokio::test]
async fn dropping_the_response_closes_its_socket() {
    let backend = Backend::new(Reply {
        frames: vec![text(
            json!({"type":"response.created","response":{"id":"began"}}),
        )],
        hold: true,
        ..Default::default()
    })
    .await;
    let (client, posts) = client(Some(&NetworkProxy::Direct));
    let response = send_turn(request(&backend.url), &client, Some(&NetworkProxy::Direct))
        .await
        .unwrap();
    drop(response);
    backend.wait_closed().await;
    assert_eq!(posts.load(Ordering::SeqCst), 0);
    assert_eq!(backend.counts(), (1, 0));
}

#[tokio::test]
async fn a_silent_turn_waits_without_replaying_and_cancellation_releases_the_socket() {
    let backend = Backend::new(Reply {
        hold: true,
        ..Default::default()
    })
    .await;
    let (client, posts) = client(Some(&NetworkProxy::Direct));
    {
        let pending = send_turn(request(&backend.url), &client, Some(&NetworkProxy::Direct));
        tokio::pin!(pending);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), pending.as_mut())
                .await
                .is_err()
        );
        assert_eq!(posts.load(Ordering::SeqCst), 0);
    }
    backend.wait_closed().await;
    assert_eq!(backend.counts(), (1, 0));
    assert_eq!(posts.load(Ordering::SeqCst), 0);
}

#[test]
fn only_streamed_codex_turns_are_eligible_and_metadata_values_are_kept() {
    assert!(is_turn(&request(
        "https://chatgpt.com/backend-api/codex/responses"
    )));
    for url in [
        "http://chatgpt.com/backend-api/codex/responses",
        "https://api.openai.com/v1/responses",
        "https://chatgpt.com/backend-api/codex/responses/compact",
        "https://chatgpt.com:8443/backend-api/codex/responses",
    ] {
        assert!(!is_turn(&request(url)), "{url}");
    }
    let mut original = request("https://chatgpt.com/backend-api/codex/responses");
    original.body = Some(
        json!({"stream":true,"client_metadata":{"x-codex-turn-state":"body-state"}})
            .to_string()
            .into_bytes(),
    );
    original
        .headers
        .insert("x-codex-turn-state".into(), "header-state".into());
    let message: Value = serde_json::from_str(&protocol::message(&original).unwrap()).unwrap();
    assert_eq!(
        message["client_metadata"]["x-codex-turn-state"],
        "body-state"
    );
    for body in [
        json!({"stream":false}),
        json!({"stream":true,"type":null}),
        json!([]),
        json!({"stream":true,"client_metadata":[]}),
    ] {
        original.body = Some(body.to_string().into_bytes());
        assert!(protocol::message(&original).is_none(), "{body}");
    }
}

struct CountingClient(AtomicUsize);
#[async_trait]
impl HttpClient for CountingClient {
    async fn stream(&self, _: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(HttpStreamingResponse {
            status: 204,
            headers: BTreeMap::new(),
            body: Box::pin(stream::empty()),
        })
    }
}

#[tokio::test]
async fn non_codex_and_non_streaming_requests_keep_the_existing_http_path() {
    let client = CountingClient(AtomicUsize::new(0));
    let ordinary = request("https://api.openai.com/v1/responses");
    assert_eq!(send(ordinary, &client, None).await.unwrap().status, 204);
    let mut no_stream = request("https://chatgpt.com/backend-api/codex/responses");
    no_stream.body = Some(b"{\"stream\":false}".to_vec());
    assert_eq!(send(no_stream, &client, None).await.unwrap().status, 204);
    assert_eq!(client.0.load(Ordering::SeqCst), 2);
}
