use std::sync::{Arc, Mutex};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_rustls::TlsAcceptor;

pub(crate) struct Server {
    pub address: std::net::SocketAddr,
    pub certificate: rustls::pki_types::CertificateDer<'static>,
    pub requests: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    pub fn url(&self, host: &str, path: &str) -> String {
        format!("https://{host}:{}{path}", self.address.port())
    }
    pub async fn start(body: &str) -> Self {
        let certificate =
            rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
                .unwrap();
        let der = certificate.cert.der().clone();
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.key_pair.serialize_der());
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![der.clone()], key.into())
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let body = body.to_string();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    break;
                };
                let acceptor = acceptor.clone();
                let body = body.clone();
                let captured = captured.clone();
                tokio::spawn(async move {
                    let Ok(mut socket) = acceptor.accept(socket).await else {
                        return;
                    };
                    let mut bytes = Vec::new();
                    let mut buffer = [0; 4096];
                    while let Ok(length) = socket.read(&mut buffer).await {
                        if length == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&buffer[..length]);
                        if bytes.windows(4).any(|part| part == b"\r\n\r\n") {
                            break;
                        }
                        if bytes.len() > 65536 {
                            return;
                        }
                    }
                    captured
                        .lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&bytes).into_owned());
                    let response=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        Self {
            address,
            certificate: der,
            requests,
            task,
        }
    }
}

impl Server {
    pub async fn proxy(target: std::net::SocketAddr) -> Self {
        let certificate = rcgen::generate_simple_self_signed(vec![
            "127.0.0.1".into(),
            "shared.nora-proxy.test".into(),
        ])
        .unwrap();
        let der = certificate.cert.der().clone();
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.key_pair.serialize_der());
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![der.clone()], key.into())
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    break;
                };
                let acceptor = acceptor.clone();
                let captured = captured.clone();
                tokio::spawn(async move {
                    let Ok(mut socket) = acceptor.accept(socket).await else {
                        return;
                    };
                    let mut bytes = Vec::new();
                    let mut byte = [0; 1];
                    while socket.read_exact(&mut byte).await.is_ok() {
                        bytes.push(byte[0]);
                        if bytes.ends_with(b"\r\n\r\n") {
                            break;
                        }
                        if bytes.len() > 65536 {
                            return;
                        }
                    }
                    let request = String::from_utf8_lossy(&bytes).into_owned();
                    if !request.starts_with("CONNECT ") {
                        return;
                    }
                    captured.lock().unwrap().push(request);
                    let Ok(mut upstream) = tokio::net::TcpStream::connect(target).await else {
                        return;
                    };
                    if socket
                        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                        .await
                        .is_err()
                    {
                        return;
                    }
                    let _ = tokio::io::copy_bidirectional(&mut socket, &mut upstream).await;
                });
            }
        });
        Self {
            address,
            certificate: der,
            requests,
            task,
        }
    }
}
