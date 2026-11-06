#![allow(dead_code)]
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;

pub struct Authority {
    cert: rcgen::Certificate,
    key: rcgen::KeyPair,
}
impl Authority {
    pub fn new() -> Self {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec![]).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.key_usages = vec![
            rcgen::KeyUsagePurpose::KeyCertSign,
            rcgen::KeyUsagePurpose::CrlSign,
        ];
        params.not_before = time::OffsetDateTime::now_utc() - time::Duration::days(3);
        params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(3650);
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "Nora isolated TLS test CA");
        Self {
            cert: params.self_signed(&key).unwrap(),
            key,
        }
    }
    pub fn der(&self) -> CertificateDer<'static> {
        self.cert.der().clone()
    }
    pub fn pem(&self) -> String {
        self.cert.pem()
    }
    pub fn acceptor(&self, names: &[&str], expired: bool) -> TlsAcceptor {
        self.acceptor_version(names, expired, rustls::DEFAULT_VERSIONS)
    }
    pub fn acceptor_version(
        &self,
        names: &[&str],
        expired: bool,
        versions: &[&'static rustls::SupportedProtocolVersion],
    ) -> TlsAcceptor {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params =
            rcgen::CertificateParams::new(names.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                .unwrap();
        params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        if expired {
            params.not_before = time::OffsetDateTime::now_utc() - time::Duration::days(3);
            params.not_after = time::OffsetDateTime::now_utc() - time::Duration::days(1);
        } else {
            params.not_before = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
            params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(30);
        }
        let cert = params.signed_by(&key, &self.cert, &self.key).unwrap();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(versions)
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.der().clone(), self.der()],
            PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
        )
        .unwrap();
        TlsAcceptor::from(Arc::new(config))
    }
}

pub struct Server {
    pub address: std::net::SocketAddr,
    pub requests: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    pub fn url(&self, host: &str) -> String {
        format!("https://{host}:{}/fixture", self.address.port())
    }
    pub fn proxy_url(&self) -> String {
        format!("https://127.0.0.1:{}", self.address.port())
    }
    pub async fn start(ca: &Authority, names: &[&str], expired: bool, body: &[u8]) -> Self {
        Self::listen(ca.acceptor(names, expired), body.to_vec(), None).await
    }
    pub async fn start_version(
        ca: &Authority,
        names: &[&str],
        expired: bool,
        body: &[u8],
        version: &'static rustls::SupportedProtocolVersion,
    ) -> Self {
        Self::listen(
            ca.acceptor_version(names, expired, &[version]),
            body.to_vec(),
            None,
        )
        .await
    }
    pub async fn proxy_version(
        ca: &Authority,
        target: std::net::SocketAddr,
        version: &'static rustls::SupportedProtocolVersion,
    ) -> Self {
        Self::listen(
            ca.acceptor_version(&["127.0.0.1", "shared.nora.test"], false, &[version]),
            vec![],
            Some(target),
        )
        .await
    }
    pub async fn proxy(ca: &Authority, target: std::net::SocketAddr) -> Self {
        Self::listen(
            ca.acceptor(&["127.0.0.1", "shared.nora.test"], false),
            vec![],
            Some(target),
        )
        .await
    }
    async fn listen(
        acceptor: TlsAcceptor,
        body: Vec<u8>,
        target: Option<std::net::SocketAddr>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let task = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                let body = body.clone();
                let count = count.clone();
                tokio::spawn(async move {
                    let Ok(mut socket) = acceptor.accept(socket).await else {
                        return;
                    };
                    let mut header = Vec::new();
                    let mut byte = [0; 1];
                    while !header.ends_with(b"\r\n\r\n") {
                        if socket.read_exact(&mut byte).await.is_err() || header.len() > 65536 {
                            return;
                        }
                        header.push(byte[0]);
                    }
                    count.fetch_add(1, Ordering::SeqCst);
                    if let Some(target) = target {
                        assert!(header.starts_with(b"CONNECT "));
                        let Ok(mut upstream) = TcpStream::connect(target).await else {
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
                    } else {
                        let header = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",body.len());
                        if socket.write_all(header.as_bytes()).await.is_ok() {
                            let _ = socket.write_all(&body).await;
                        }
                    }
                });
            }
        });
        Self {
            address,
            requests,
            task,
        }
    }
}

pub async fn check_http_trust(
    builder: impl Fn(Option<&str>, Vec<CertificateDer<'static>>) -> reqwest::ClientBuilder,
) {
    for version in [&rustls::version::TLS12, &rustls::version::TLS13] {
        check_http_version(&builder, version).await;
    }
    // The proxy's outer TLS and target's inner TLS negotiate independently.
    for (outer, inner) in [
        (&rustls::version::TLS12, &rustls::version::TLS13),
        (&rustls::version::TLS13, &rustls::version::TLS12),
    ] {
        let ca = Authority::new();
        let proxy_ca = Authority::new();
        let target =
            Server::start_version(&ca, &["target.nora.test"], false, b"mixed", inner).await;
        let proxy = Server::proxy_version(&proxy_ca, target.address, outer).await;
        let response = builder(Some(&proxy.proxy_url()), vec![ca.der(), proxy_ca.der()])
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap()
            .get(target.url("target.nora.test"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "mixed");
        assert_eq!(proxy.requests.load(Ordering::SeqCst), 1);
    }
}

async fn check_http_version(
    builder: &impl Fn(Option<&str>, Vec<CertificateDer<'static>>) -> reqwest::ClientBuilder,
    version: &'static rustls::SupportedProtocolVersion,
) {
    let target_ca = Authority::new();
    let proxy_ca = Authority::new();
    let target = Server::start_version(
        &target_ca,
        &["localhost", "127.0.0.1", "target.nora.test"],
        false,
        b"trusted",
        version,
    )
    .await;
    let target_url = target.url("localhost");
    let client = |roots| builder(None, roots).timeout(std::time::Duration::from_secs(5));
    assert!(
        client(vec![])
            .build()
            .unwrap()
            .get(&target_url)
            .send()
            .await
            .is_err(),
        "untrusted CA must be rejected ({version:?})"
    );
    let trusted = client(vec![target_ca.der()]).build().unwrap();
    assert_eq!(
        trusted
            .get(&target_url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "trusted"
    );
    assert!(
        client(vec![])
            .build()
            .unwrap()
            .get(&target_url)
            .send()
            .await
            .is_err(),
        "trust must not escape its client"
    );
    let wrong_host =
        Server::start_version(&target_ca, &["other.invalid"], false, b"bad", version).await;
    assert!(
        trusted
            .get(wrong_host.url("localhost"))
            .send()
            .await
            .is_err(),
        "trusted CA must still require matching hostname"
    );
    let expired = Server::start_version(&target_ca, &["localhost"], true, b"bad", version).await;
    assert!(
        trusted.get(expired.url("localhost")).send().await.is_err(),
        "trusted CA must still require valid dates"
    );
    let proxy = Server::proxy_version(&proxy_ca, target.address, version).await;
    let proxy_url = proxy.proxy_url();
    let target_url = target.url("target.nora.test");
    let via = |roots| builder(Some(&proxy_url), roots).timeout(std::time::Duration::from_secs(5));
    assert!(
        via(vec![target_ca.der()])
            .build()
            .unwrap()
            .get(&target_url)
            .send()
            .await
            .is_err(),
        "target trust must not bypass untrusted HTTPS proxy"
    );
    assert_eq!(proxy.requests.load(Ordering::SeqCst), 0);
    assert!(
        via(vec![proxy_ca.der()])
            .build()
            .unwrap()
            .get(&target_url)
            .send()
            .await
            .is_err(),
        "proxy trust must not bypass untrusted target"
    );
    let trusted_both = via(vec![target_ca.der(), proxy_ca.der()]).build().unwrap();
    assert_eq!(
        trusted_both
            .get(&target_url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "trusted"
    );
    assert_eq!(proxy.requests.load(Ordering::SeqCst), 2);
}
