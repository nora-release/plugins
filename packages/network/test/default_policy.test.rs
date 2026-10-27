use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn ai_default_client_retains_redirects_while_sdk_client_retains_its_no_redirect_policy() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/start", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut paths = Vec::new();
        for _ in 0..3 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut header = Vec::new();
            let mut byte = [0; 1];
            while !header.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).await.unwrap();
                header.push(byte[0]);
            }
            let head = String::from_utf8(header).unwrap();
            let path = head.split_whitespace().nth(1).unwrap().to_owned();
            let response: &[u8] = if path == "/start" {
                b"HTTP/1.1 302 Found\r\nLocation: /done\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            } else {
                b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ndone"
            };
            paths.push(path);
            socket.write_all(response).await.unwrap();
        }
        paths
    });
    let response = default_http_client()
        .unwrap()
        .get(&url)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), "done");
    let response = http_client(Some(&NetworkProxy::Direct))
        .unwrap()
        .get(&url)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FOUND);
    assert_eq!(server.await.unwrap(), ["/start", "/done", "/start"]);
}
