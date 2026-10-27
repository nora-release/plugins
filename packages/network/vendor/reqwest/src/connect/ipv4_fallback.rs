//! Direct HTTPS recovery belongs below HTTP: only a failed, byte-free IPv6 TLS
//! handshake can cause another connection, so no request headers or body can
//! have reached the peer. Proxy connectors never enter this module.
use super::{BoxError, HttpConnector};
use http::Uri;
use hyper_util::client::legacy::connect::{Connected, Connection};
use hyper_util::rt::TokioIo;
use rustls_pki_types::ServerName;
use std::io::{self, IoSlice};
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};
use std::task::{Context, Poll};
use std::time::Instant;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::{client::TlsStream, TlsConnector};
use tower_service::Service;

const IPV6: u8 = 1;
const RECEIVED: u8 = 2;
const DISCONNECTED: u8 = 4;
const SHUTDOWN: u8 = 8;

tokio::task_local! {
    static IPV4_ONLY: bool;
}

/// The filter is scoped to this connection's future, not a process-wide DNS
/// override: concurrent proxy, other-port and other-client dials stay untouched.
pub(crate) fn ipv4_only() -> bool {
    IPV4_ONLY.try_with(|enabled| *enabled).unwrap_or(false)
}

#[path = "ipv4_preference.rs"]
mod ipv4_preference;
use ipv4_preference::preferences;

pub(super) async fn connect(
    http: HttpConnector,
    tls: Arc<rustls::ClientConfig>,
    destination: Uri,
) -> Result<TlsStream<ObservedIo>, BoxError> {
    let host = destination.host().ok_or("no host in url")?;
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    let server_name = ServerName::try_from(host.to_owned())?;
    // A literal IPv6 address has no IPv4 counterpart to resolve. In particular,
    // never retry it over IPv6 when the IPv4 DNS filter cannot apply.
    let can_fallback = host.parse::<IpAddr>().is_err();
    let key = format!(
        "{}:{}",
        host.to_ascii_lowercase(),
        destination.port_u16().unwrap_or(443)
    );
    let use_ipv4 = can_fallback && preferences().lock().unwrap().contains(&key, Instant::now());
    let (result, disconnected) = attempt(
        http.clone(),
        tls.clone(),
        destination.clone(),
        server_name.clone(),
        use_ipv4,
    )
    .await;
    if result.is_ok() || !can_fallback || use_ipv4 || !disconnected {
        return result;
    }
    preferences().lock().unwrap().remember(key, Instant::now());
    attempt(http, tls, destination, server_name, true).await.0
}

async fn attempt(
    mut http: HttpConnector,
    tls: Arc<rustls::ClientConfig>,
    destination: Uri,
    server_name: ServerName<'static>,
    ipv4: bool,
) -> (Result<TlsStream<ObservedIo>, BoxError>, bool) {
    let evidence = Arc::new(AtomicU8::new(0));
    let result = IPV4_ONLY
        .scope(ipv4, async {
            let socket = http.call(destination).await?;
            if socket.inner().peer_addr()?.is_ipv6() {
                evidence.store(IPV6, Ordering::Relaxed);
            }
            let socket = ObservedIo {
                inner: TokioIo::new(socket),
                evidence: evidence.clone(),
            };
            TlsConnector::from(tls)
                .connect(server_name, socket)
                .await
                .map_err(Into::into)
        })
        .await;
    (
        result,
        evidence.load(Ordering::Relaxed) == IPV6 | DISCONNECTED,
    )
}

pub(super) struct ObservedIo {
    inner: TokioIo<TokioIo<TcpStream>>,
    evidence: Arc<AtomicU8>,
}

impl ObservedIo {
    pub(super) fn connected(&self) -> Connected {
        self.inner.inner().connected()
    }

    pub(super) fn set_nodelay(&self, value: bool) -> io::Result<()> {
        self.inner.inner().inner().set_nodelay(value)
    }

    fn disconnected(&self, error: Option<&io::Error>) {
        if error.is_some_and(|error| {
            !matches!(
                error.kind(),
                io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::BrokenPipe
            )
        }) {
            return;
        }
        let _ = self.evidence.compare_exchange(
            IPV6,
            IPV6 | DISCONNECTED,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }
}

impl AsyncRead for ObservedIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buffer.filled().len();
        let had_capacity = buffer.remaining() > 0;
        let result = Pin::new(&mut self.inner).poll_read(cx, buffer);
        if buffer.filled().len() > before {
            self.evidence.fetch_or(RECEIVED, Ordering::Relaxed);
        } else if let Poll::Ready(result) = &result {
            match result {
                Ok(()) if had_capacity => self.disconnected(None),
                Err(error) => self.disconnected(Some(error)),
                _ => (),
            }
        }
        result
    }
}

impl AsyncWrite for ObservedIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write(cx, bytes);
        if let Poll::Ready(Err(error)) = &result {
            self.disconnected(Some(error));
        }
        result
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write_vectored(cx, bytes);
        if let Poll::Ready(Err(error)) = &result {
            self.disconnected(Some(error));
        }
        result
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let result = Pin::new(&mut self.inner).poll_flush(cx);
        if let Poll::Ready(Err(error)) = &result {
            self.disconnected(Some(error));
        }
        result
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.evidence.fetch_or(SHUTDOWN, Ordering::Relaxed);
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
