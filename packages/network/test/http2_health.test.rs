use super::*;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

const PING: u8 = 6;
const HEADERS: u8 = 1;
const SETTINGS: u8 = 4;
const ACK: u8 = 1;

struct Frame {
    kind: u8,
    flags: u8,
    stream: u32,
    payload: Vec<u8>,
}

async fn read_frame(socket: &mut TcpStream) -> std::io::Result<Frame> {
    let mut header = [0; 9];
    socket.read_exact(&mut header).await?;
    let size = u32::from_be_bytes([0, header[0], header[1], header[2]]) as usize;
    assert!(size <= 16_384, "unexpected oversized client frame");
    let mut payload = vec![0; size];
    socket.read_exact(&mut payload).await?;
    Ok(Frame {
        kind: header[3],
        flags: header[4],
        stream: u32::from_be_bytes(header[5..9].try_into().unwrap()) & 0x7fff_ffff,
        payload,
    })
}

async fn write_frame(socket: &mut TcpStream, kind: u8, flags: u8, stream: u32, data: &[u8]) {
    let mut bytes = Vec::with_capacity(data.len() + 9);
    bytes.extend_from_slice(&(data.len() as u32).to_be_bytes()[1..]);
    bytes.extend_from_slice(&[kind, flags]);
    bytes.extend_from_slice(&stream.to_be_bytes());
    bytes.extend_from_slice(data);
    socket.write_all(&bytes).await.unwrap();
}

async fn accept_request(listener: &TcpListener) -> (TcpStream, u32) {
    let (mut socket, _) = listener.accept().await.unwrap();
    let mut preface = [0; 24];
    socket.read_exact(&mut preface).await.unwrap();
    assert_eq!(&preface, b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n");
    let settings = read_frame(&mut socket).await.unwrap();
    assert_eq!(settings.kind, SETTINGS);
    write_frame(&mut socket, SETTINGS, 0, 0, &[]).await;
    write_frame(&mut socket, SETTINGS, ACK, 0, &[]).await;
    loop {
        let frame = read_frame(&mut socket).await.unwrap();
        if frame.kind == HEADERS {
            return (socket, frame.stream);
        }
    }
}

async fn send_response(socket: &mut TcpStream, stream: u32) {
    // HPACK's static table index 8 is :status = 200.
    write_frame(socket, HEADERS, 4, stream, &[0x88]).await;
    write_frame(socket, 0, 1, stream, b"recovered").await;
}

fn sdk_client() -> reqwest::Client {
    http_client_builder(Some(&NetworkProxy::Direct))
        .unwrap()
        .http2_prior_knowledge()
        .build()
        .unwrap()
}

#[tokio::test]
async fn http2_health_detects_a_silent_active_connection_and_reconnects() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/generation", listener.local_addr().unwrap());
    let (ping_tx, ping_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = accept_request(&listener).await;
        let mut ping_tx = Some(ping_tx);
        while let Ok(frame) = read_frame(&mut socket).await {
            if frame.kind == PING && frame.flags == 0 {
                if let Some(tx) = ping_tx.take() {
                    tx.send(frame.payload.len()).unwrap();
                }
                // A dead peer accepts bytes but never acknowledges the ping.
            }
        }
        assert!(ping_tx.is_none(), "connection ended without a health ping");
        let (mut socket, stream) = accept_request(&listener).await;
        send_response(&mut socket, stream).await;
        while read_frame(&mut socket).await.is_ok() {}
    });
    let client = sdk_client();
    let response = tokio::time::timeout(
        Duration::from_secs(36),
        client.post(&url).body("message").send(),
    )
    .await;
    assert!(
        response.is_ok(),
        "a silent HTTP/2 connection must fail after its unanswered ping"
    );
    assert!(response.unwrap().is_err());
    assert_eq!(ping_rx.await.unwrap(), 8);
    let response = tokio::time::timeout(Duration::from_secs(5), client.get(&url).send())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "recovered");
    drop(client);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn http2_health_keeps_a_long_generation_with_responsive_pings_alive() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/slow", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, stream) = accept_request(&listener).await;
        let respond_at = tokio::time::Instant::now() + Duration::from_secs(34);
        let mut pings = 0;
        loop {
            tokio::select! {
                frame = read_frame(&mut socket) => {
                    let frame = frame.unwrap();
                    if frame.kind == PING && frame.flags == 0 {
                        pings += 1;
                        write_frame(&mut socket, PING, ACK, 0, &frame.payload).await;
                    }
                }
                _ = tokio::time::sleep_until(respond_at) => break,
            }
        }
        send_response(&mut socket, stream).await;
        while read_frame(&mut socket).await.is_ok() {}
        pings
    });
    let client = configure_client(reqwest::Client::builder())
        .unwrap()
        .no_proxy()
        .http2_prior_knowledge()
        .build()
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(39), client.get(&url).send())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "recovered");
    drop(client);
    let pings = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    assert!(
        pings > 0,
        "a long live generation must still receive health pings"
    );
}

#[tokio::test]
async fn http2_health_checks_idle_connections_before_the_next_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/idle", listener.local_addr().unwrap());
    let (closed_tx, closed_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, stream) = accept_request(&listener).await;
        send_response(&mut socket, stream).await;
        let mut saw_ping = false;
        while let Ok(frame) = read_frame(&mut socket).await {
            if frame.kind == PING && frame.flags == 0 {
                saw_ping = true;
            }
        }
        closed_tx.send(saw_ping).unwrap();
        let (mut socket, stream) = accept_request(&listener).await;
        send_response(&mut socket, stream).await;
        while read_frame(&mut socket).await.is_ok() {}
    });
    let client = sdk_client();
    assert_eq!(
        client.get(&url).send().await.unwrap().text().await.unwrap(),
        "recovered"
    );
    let closed = tokio::time::timeout(Duration::from_secs(36), closed_rx).await;
    assert!(
        closed.is_ok(),
        "an idle HTTP/2 connection must be checked before reusing it"
    );
    assert!(closed.unwrap().unwrap());
    assert_eq!(
        client.get(&url).send().await.unwrap().text().await.unwrap(),
        "recovered"
    );
    drop(client);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}
