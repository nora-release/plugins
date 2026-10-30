use super::*;
use nora_ai::{collect_http_body_text_limited, HttpMethod};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use std::sync::{Arc, Mutex};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::{JoinHandle, JoinSet},
};
use tokio_rustls::{server::TlsStream, TlsAcceptor};

#[derive(Debug)]
struct Http1Provider;

impl crate::ProviderAdapter for Http1Provider {
    fn http1_hosts(&self) -> &'static [&'static str] {
        &["localhost"]
    }
}

struct Server {
    address: std::net::SocketAddr,
    root: CertificateDer<'static>,
    protocols: Arc<Mutex<Vec<Option<Vec<u8>>>>>,
    task: JoinHandle<()>,
}

impl Server {
    async fn start() -> Self {
        let certificate =
            rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
                .unwrap();
        let root = certificate.cert.der().clone();
        let key = PrivatePkcs8KeyDer::from(certificate.key_pair.serialize_der());
        let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![root.clone()], key.into())
        .unwrap();
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let protocols = Arc::new(Mutex::new(Vec::new()));
        let captured = protocols.clone();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    connection = listener.accept() => {
                        let (socket, _) = connection.unwrap();
                        let acceptor = acceptor.clone();
                        let captured = captured.clone();
                        connections.spawn(async move {
                            let Ok(mut socket) = acceptor.accept(socket).await else {
                                return;
                            };
                            let protocol = socket.get_ref().1.alpn_protocol().map(<[u8]>::to_vec);
                            captured.lock().unwrap().push(protocol.clone());
                            if protocol.as_deref() == Some(b"h2".as_slice()) {
                                respond_h2(&mut socket).await;
                            } else {
                                respond_http1(&mut socket).await;
                            }
                        });
                    }
                    Some(result) = connections.join_next(), if !connections.is_empty() => {
                        result.unwrap();
                    }
                }
            }
        });
        Self {
            address,
            root,
            protocols,
            task,
        }
    }

    fn url(&self, host: &str) -> String {
        format!("https://{host}:{}/fixture", self.address.port())
    }

    fn definition(&self) -> Definition {
        let mut definition = Definition::from_fields(serde_json::json!({
            "id":"http1-fixture", "name":"HTTP1 fixture", "api":"openai-responses",
            "baseURL":self.url("localhost"), "config":{"keyProvider":"fixture"}
        }))
        .unwrap();
        definition.adapter = Arc::new(Http1Provider);
        definition
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn respond_http1(socket: &mut TlsStream<TcpStream>) {
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        let Ok(byte) = socket.read_u8().await else {
            return;
        };
        headers.push(byte);
        assert!(headers.len() < 65536);
    }
    assert!(headers.starts_with(b"GET /fixture HTTP/1.1\r\n"));
    socket
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nfixture")
        .await
        .unwrap();
}

async fn respond_h2(socket: &mut TlsStream<TcpStream>) {
    let mut preface = [0; 24];
    socket.read_exact(&mut preface).await.unwrap();
    assert_eq!(&preface, b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n");
    socket.write_all(&h2_frame(4, 0, 0, &[])).await.unwrap();
    loop {
        let mut header = [0; 9];
        socket.read_exact(&mut header).await.unwrap();
        let length = u32::from_be_bytes([0, header[0], header[1], header[2]]) as usize;
        assert!(length <= 65536);
        let mut payload = vec![0; length];
        socket.read_exact(&mut payload).await.unwrap();
        if header[3] == 4 && header[4] & 1 == 0 {
            socket.write_all(&h2_frame(4, 1, 0, &[])).await.unwrap();
        }
        if header[3] == 1 {
            let stream = u32::from_be_bytes(header[5..9].try_into().unwrap()) & 0x7fff_ffff;
            // HPACK's static table index 8 is :status 200; this fixture only
            // needs one header block followed by one complete data frame.
            let mut response = h2_frame(1, 4, stream, &[0x88]);
            response.extend(h2_frame(0, 1, stream, b"fixture"));
            socket.write_all(&response).await.unwrap();
            return;
        }
    }
}

fn h2_frame(kind: u8, flags: u8, stream: u32, payload: &[u8]) -> Vec<u8> {
    let mut bytes = (payload.len() as u32).to_be_bytes()[1..].to_vec();
    bytes.extend([kind, flags]);
    bytes.extend(stream.to_be_bytes());
    bytes.extend(payload);
    bytes
}

async fn request(client: &Client, url: String) {
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client.stream(HttpRequest::new(HttpMethod::Get, url)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(
        collect_http_body_text_limited(response.body, 1024)
            .await
            .unwrap(),
        "fixture"
    );
}

#[tokio::test]
async fn oauth_host_uses_http1_even_when_the_server_prefers_h2() {
    use crate::ProviderAdapter;
    assert_eq!(
        crate::openai_provider::OpenAI.http1_hosts(),
        &["chatgpt.com"]
    );
    let server = Server::start().await;
    TEST_ROOTS
        .scope(vec![server.root.clone()], async {
            let client = Client::new(
                Some(&server.definition()),
                Some(&NetworkProxy::Direct),
                false,
            )
            .unwrap();
            request(&client, server.url("localhost")).await;
            assert_eq!(*server.protocols.lock().unwrap(), vec![None]);
        })
        .await;
}

#[tokio::test]
async fn api_keys_and_other_hosts_keep_the_shared_protocol_policy() {
    let server = Server::start().await;
    TEST_ROOTS
        .scope(vec![server.root.clone()], async {
            let definition = server.definition();
            let api = Client::new(Some(&definition), Some(&NetworkProxy::Direct), true).unwrap();
            assert!(api.http1.is_none());
            request(&api, server.url("localhost")).await;
            let oauth = Client::new(Some(&definition), Some(&NetworkProxy::Direct), false).unwrap();
            request(&oauth, server.url("127.0.0.1")).await;
            let default = nora_network::tls::http_client_config(std::iter::empty()).unwrap();
            let protocol = default.alpn_protocols.first().cloned();
            assert_eq!(
                *server.protocols.lock().unwrap(),
                vec![protocol.clone(), protocol]
            );
        })
        .await;
}

#[tokio::test]
async fn http1_keeps_certificate_verification_and_test_roots_stay_in_their_task() {
    let server = Server::start().await;
    TEST_ROOTS
        .scope(vec![server.root.clone()], async {
            let client = Client::new(
                Some(&server.definition()),
                Some(&NetworkProxy::Direct),
                false,
            )
            .unwrap();
            request(&client, server.url("localhost")).await;
            let definition = server.definition();
            let url = server.url("localhost");
            let untrusted = tokio::spawn(async move {
                let client =
                    Client::new(Some(&definition), Some(&NetworkProxy::Direct), false).unwrap();
                client
                    .stream(HttpRequest::new(HttpMethod::Get, url))
                    .await
                    .is_err()
            })
            .await
            .unwrap();
            assert!(untrusted);
        })
        .await;
    let client = Client::new(
        Some(&server.definition()),
        Some(&NetworkProxy::Direct),
        false,
    )
    .unwrap();
    assert!(client
        .stream(HttpRequest::new(HttpMethod::Get, server.url("localhost")))
        .await
        .is_err());
    assert_eq!(server.protocols.lock().unwrap().len(), 1);
}
