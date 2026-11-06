use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[tokio::test]
async fn native_http_preserves_all_set_cookie_headers_without_following_redirects() {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = [0u8; 4096];
        let _ = socket.read(&mut bytes).await.unwrap();
        socket.write_all(b"HTTP/1.1 302 Found\r\nLocation: /must-not-follow\r\nSet-Cookie: first=one; Path=/\r\nSet-Cookie: second=two; Path=/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
    });
    let response = Http
        .send(
            Request::get(format!("http://{address}/start"), BTreeMap::new()),
            None,
            Duration::from_secs(3),
        )
        .await
        .unwrap();
    assert_eq!(response.status, 302);
    assert_eq!(response.set_cookies.len(), 2);
    assert!(response
        .set_cookies
        .iter()
        .any(|value| value.starts_with("first=")));
    assert!(response
        .set_cookies
        .iter()
        .any(|value| value.starts_with("second=")));
    server.await.unwrap();
}
#[tokio::test]
async fn oversized_account_json_is_an_error_and_does_not_return_a_partial_success() {
    let response = Reply {
        status: 200,
        headers: BTreeMap::new(),
        set_cookies: Vec::new(),
        body: Box::pin(futures::stream::iter(vec![
            Ok(vec![1; 10]),
            Ok(vec![2; 10]),
        ])),
    };
    assert!(collect(response, 15).await.is_err());
}
