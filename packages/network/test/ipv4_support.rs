use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};
use tokio_rustls::TlsAcceptor;

#[derive(Clone)]
pub enum Behavior {
    Eof,
    Reset,
    PartialTls,
    TlsAlert,
    Stall,
    PlainHttpEof,
    Tls(TlsAcceptor, bool),
}

pub struct Endpoint {
    pub address: SocketAddr,
    pub accepts: Arc<AtomicUsize>,
    pub requests: Arc<Mutex<Vec<Vec<u8>>>>,
    task: JoinHandle<()>,
}

impl Endpoint {
    pub async fn v4(behavior: Behavior) -> Self {
        Self::listen(
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap(),
            behavior,
        )
    }

    pub async fn v6(port: u16, behavior: Behavior) -> Self {
        Self::listen(
            TcpListener::bind((Ipv6Addr::LOCALHOST, port))
                .await
                .unwrap(),
            behavior,
        )
    }

    fn listen(listener: TcpListener, behavior: Behavior) -> Self {
        let address = listener.local_addr().unwrap();
        let accepts = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let count = accepts.clone();
        let log = requests.clone();
        let task = tokio::spawn(async move {
            let mut tasks = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else { break; };
                        count.fetch_add(1, Ordering::SeqCst);
                        let behavior = behavior.clone();
                        let log = log.clone();
                        tasks.spawn(async move { serve(socket, behavior, log).await });
                    }
                    result = tasks.join_next(), if !tasks.is_empty() => {
                        if let Some(result) = result { result.unwrap(); }
                    }
                }
            }
        });
        Self {
            address,
            accepts,
            requests,
            task,
        }
    }

    pub fn count(&self) -> usize {
        self.accepts.load(Ordering::SeqCst)
    }

    pub fn bodies(&self) -> Vec<Vec<u8>> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(mut socket: TcpStream, behavior: Behavior, requests: Arc<Mutex<Vec<Vec<u8>>>>) {
    match behavior {
        Behavior::Tls(acceptor, reply) => {
            let Ok(mut socket) = acceptor.accept(socket).await else {
                return;
            };
            let Some(body) = read_request(&mut socket).await else {
                return;
            };
            requests.lock().unwrap().push(body);
            if reply {
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nreply",
                    )
                    .await;
                let _ = socket.shutdown().await;
            }
        }
        Behavior::PlainHttpEof => {
            if let Some(body) = read_request(&mut socket).await {
                requests.lock().unwrap().push(body);
            }
        }
        behavior => {
            let mut head = [0; 5];
            if socket.read_exact(&mut head).await.is_err() {
                return;
            }
            assert_eq!(head[0], 22, "fixture expects a TLS handshake record");
            let length = u16::from_be_bytes([head[3], head[4]]) as usize;
            let mut hello = vec![0; length];
            if socket.read_exact(&mut hello).await.is_err() {
                return;
            }
            match behavior {
                Behavior::Eof => {
                    let _ = socket.shutdown().await;
                }
                Behavior::Reset => {
                    socket2::SockRef::from(&socket)
                        .set_linger(Some(Duration::ZERO))
                        .unwrap();
                }
                Behavior::PartialTls => {
                    let _ = socket.write_all(&[22]).await;
                    let _ = socket.shutdown().await;
                }
                Behavior::TlsAlert => {
                    let _ = socket.write_all(&[21, 3, 3, 0, 2, 2, 40]).await;
                    let _ = socket.shutdown().await;
                }
                Behavior::Stall => std::future::pending::<()>().await,
                _ => unreachable!(),
            }
        }
    }
}

async fn read_request(socket: &mut (impl AsyncRead + Unpin)) -> Option<Vec<u8>> {
    let mut header = Vec::new();
    let mut byte = [0; 1];
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() >= 65536 || socket.read_exact(&mut byte).await.is_err() {
            return None;
        }
        header.push(byte[0]);
    }
    let text = String::from_utf8(header).unwrap();
    let length = text
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| value.trim().parse::<usize>().unwrap())
        .unwrap_or(0);
    assert!(
        !text
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked"),
        "streaming fixture declares its length"
    );
    let mut body = vec![0; length];
    socket.read_exact(&mut body).await.ok()?;
    Some(body)
}
