use super::*;
use futures::{SinkExt, StreamExt};
use std::sync::{Arc, Mutex};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::{JoinHandle, JoinSet},
};
use tokio_rustls::{
    rustls::{
        self,
        pki_types::{CertificateDer, PrivatePkcs8KeyDer},
    },
    TlsAcceptor,
};
use tokio_tungstenite::{
    tungstenite::{handshake::derive_accept_key, protocol::Role},
    WebSocketStream,
};

struct Backend {
    url: String,
    certificate: CertificateDer<'static>,
    protocols: Arc<Mutex<Vec<Option<Vec<u8>>>>>,
    task: JoinHandle<()>,
}

impl Backend {
    async fn new() -> Self {
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let der = certificate.cert.der().clone();
        let key = PrivatePkcs8KeyDer::from(certificate.key_pair.serialize_der());
        let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![der.clone()], key.into())
        .unwrap();
        tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let acceptor = TlsAcceptor::from(Arc::new(tls));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "https://localhost:{}/backend-api/codex/responses",
            listener.local_addr().unwrap().port(),
        );
        let protocols = Arc::new(Mutex::new(Vec::new()));
        let selected = protocols.clone();
        let task = tokio::spawn(async move {
            let mut tasks = JoinSet::new();
            loop {
                tokio::select! {
                    connection = listener.accept() => {
                        let (stream, _) = connection.unwrap();
                        let acceptor = acceptor.clone();
                        let selected = selected.clone();
                        tasks.spawn(async move {
                            let Ok(mut stream) = acceptor.accept(stream).await else {
                                return;
                            };
                            let alpn = stream.get_ref().1.alpn_protocol().map(Vec::from);
                            selected.lock().unwrap().push(alpn);
                            let mut header = Vec::new();
                            while !header.ends_with(b"\r\n\r\n") {
                                let Ok(byte) = stream.read_u8().await else {
                                    return;
                                };
                                header.push(byte);
                                if header.len() > 65536 {
                                    return;
                                }
                            }
                            let text = String::from_utf8(header).unwrap();
                            assert!(text.lines().next().unwrap().ends_with("HTTP/1.1"));
                            let key = text.lines().filter_map(|line| line.split_once(':'))
                                .find(|(name, _)| name.eq_ignore_ascii_case("sec-websocket-key"))
                                .unwrap().1.trim();
                            let proof = derive_accept_key(key.as_bytes());
                            let response = format!("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {proof}\r\n\r\n");
                            stream.write_all(response.as_bytes()).await.unwrap();
                            let mut socket = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
                            let message = socket.next().await.unwrap().unwrap();
                            let value: serde_json::Value = serde_json::from_slice(&message.into_data()).unwrap();
                            assert_eq!(value["type"], "response.create");
                            socket.send(Message::Text(r#"{"type":"response.created","response":{"id":"tls-response"}}"#.into())).await.unwrap();
                            socket.send(Message::Text(r#"{"type":"response.completed","response":{"id":"tls-response"}}"#.into())).await.unwrap();
                        });
                    }
                    Some(result) = tasks.join_next(), if !tasks.is_empty() => result.unwrap(),
                }
            }
        });
        Self {
            url,
            certificate: der,
            protocols,
            task,
        }
    }

    fn request(&self) -> HttpRequest {
        HttpRequest::new(HttpMethod::Post, &self.url)
            .with_header("authorization", "Bearer tls-test-token")
            .with_body(br#"{"model":"test","stream":true,"input":"ping"}"#.to_vec())
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn https_upgrade_keeps_http1_when_the_server_prefers_h2() {
    let backend = Backend::new().await;
    let request = backend.request();
    let response = socket::EXTRA_ROOTS
        .scope(
            vec![backend.certificate.clone()],
            websocket(
                &request,
                &protocol::message(&request).unwrap(),
                &backend.url,
                Some(&NetworkProxy::Direct),
            ),
        )
        .await
        .unwrap()
        .expect("TLS must permit an HTTP/1.1 WebSocket upgrade");
    let text = nora_ai::collect_http_body_text_limited(response.body, 4096)
        .await
        .unwrap();
    assert!(text.contains("event: response.completed"));
    let protocols = backend.protocols.lock().unwrap();
    assert_eq!(protocols.len(), 1);
    assert!(protocols[0]
        .as_deref()
        .is_none_or(|protocol| protocol == b"http/1.1"));
}

#[tokio::test]
async fn isolated_tls_test_roots_do_not_disable_hostname_or_untrusted_chain_checks() {
    let backend = Backend::new().await;
    let mut request = backend.request();
    let timeout = Duration::from_secs(5);
    assert!(
        socket::connect(&request, Some(&NetworkProxy::Direct), timeout)
            .await
            .is_err()
    );
    request.url = request.url.replace("localhost", "127.0.0.1");
    let wrong_host = socket::EXTRA_ROOTS
        .scope(
            vec![backend.certificate.clone()],
            socket::connect(&request, Some(&NetworkProxy::Direct), timeout),
        )
        .await;
    assert!(wrong_host.is_err());
    assert!(backend.protocols.lock().unwrap().is_empty());
}
