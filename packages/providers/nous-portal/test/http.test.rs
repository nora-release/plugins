use super::*;
use futures::{Stream, StreamExt};
use nora_ai::{HttpMethod, ReqwestHttpClient};
use std::{
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{Context, Poll},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
struct PendingBody {
    dropped: Arc<AtomicBool>,
    first: bool,
}
impl Stream for PendingBody {
    type Item = Result<bytes::Bytes, nora_ai::HttpClientError>;
    fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.first {
            self.first = false;
            Poll::Ready(Some(Ok(b"data: first\n\n".as_slice().into())))
        } else {
            Poll::Pending
        }
    }
}
impl Drop for PendingBody {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}
#[tokio::test]
async fn streaming_delivers_first_delta_before_completion_and_drop_releases_upstream() {
    let dropped = Arc::new(AtomicBool::new(false));
    let upstream = HttpStreamingResponse {
        status: 200,
        headers: BTreeMap::new(),
        body: Box::pin(PendingBody {
            dropped: dropped.clone(),
            first: true,
        }),
    };
    let mut response = relay(upstream);
    assert_eq!(
        response.body.next().await.unwrap().unwrap(),
        b"data: first\n\n"
    );
    assert!(!dropped.load(Ordering::SeqCst));
    drop(response);
    assert!(dropped.load(Ordering::SeqCst));
}
#[tokio::test]
async fn actual_http_transport_decodes_gzip_before_dropping_wire_headers() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        let _ = socket.read(&mut request).await.unwrap();
        let gzip: &[u8] = &[
            31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 75, 73, 44, 73, 180, 82, 72, 73, 77, 206, 79, 73,
            77, 225, 226, 2, 0, 81, 74, 183, 173, 15, 0, 0, 0,
        ];
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",gzip.len()).as_bytes()).await.unwrap();
        socket.write_all(gzip).await.unwrap();
    });
    let client = ReqwestHttpClient::with_client(
        nora_provider_plugin_sdk::http_client(Some(
            &nora_provider_plugin_sdk::NetworkProxy::Direct,
        ))
        .unwrap(),
    );
    let response = send(
        &client,
        HttpRequest::new(HttpMethod::Get, format!("http://{address}/stream")),
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    let response = relay(response);
    assert!(!response.headers.contains_key("content-encoding"));
    assert!(!response.headers.contains_key("content-length"));
    assert_eq!(
        response
            .body
            .map(|chunk| chunk.unwrap())
            .collect::<Vec<_>>()
            .await
            .concat(),
        b"data: decoded\n\n"
    );
    server.await.unwrap();
}
