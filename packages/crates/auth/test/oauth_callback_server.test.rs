use super::*;
use tokio::net::TcpStream;

#[test]
fn callback_server_can_publish_redirect_host_separately_from_bind_host() {
    let config = OAuthCallbackServerConfig::with_path(1455, "/auth/callback")
        .with_redirect_host("localhost");

    assert_eq!(config.host, "127.0.0.1");
    assert_eq!(config.redirect_uri(), "http://localhost:1455/auth/callback");
}

#[tokio::test]
async fn callback_server_returns_query_params_and_success_page() {
    let mut server = OAuthCallbackServer::new(0);
    server.start().await.unwrap();
    let port = server.config.port;

    let request = send_request(port, "GET /callback?code=abc&state=xyz HTTP/1.1\r\n\r\n");
    let wait = server.wait_for_callback();
    let (response, params) = tokio::join!(request, wait);

    assert!(response.unwrap().contains("200 OK"));
    assert_eq!(params.unwrap().get("code").map(String::as_str), Some("abc"));
}

#[tokio::test]
async fn callback_server_returns_oauth_error_for_error_query() {
    let mut server = OAuthCallbackServer::new(0);
    server.start().await.unwrap();
    let port = server.config.port;

    let request = send_request(port, "GET /callback?error=denied HTTP/1.1\r\n\r\n");
    let wait = server.wait_for_callback();
    let (response, error) = tokio::join!(request, wait);

    assert!(response.unwrap().contains("400 Bad Request"));
    assert_eq!(
        error.unwrap_err(),
        OAuthError::InvalidResponse("denied".to_string())
    );
}

#[tokio::test]
async fn callback_server_rejects_unknown_path_without_resolving() {
    let mut server = OAuthCallbackServer::new(0);
    server.start().await.unwrap();

    let response = send_request(server.config.port, "GET /wrong?code=abc HTTP/1.1\r\n\r\n")
        .await
        .unwrap();

    assert!(response.contains("404 Not Found"));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), server.wait_for_callback())
            .await
            .is_err()
    );
    server.cancel();
}

#[tokio::test]
async fn xai_private_network_preflight_does_not_resolve_callback() {
    let mut server = OAuthCallbackServer::new(0);
    server.start().await.unwrap();
    let port = server.config.port;

    let response = send_request(
        port,
        "OPTIONS /callback HTTP/1.1\r\nOrigin: https://accounts.x.ai\r\nAccess-Control-Request-Method: GET\r\nAccess-Control-Request-Headers: content-type\r\nAccess-Control-Request-Private-Network: true\r\n\r\n",
    )
    .await
    .unwrap();

    assert!(response.contains("204 No Content"));
    assert!(response.contains("Access-Control-Allow-Origin: https://accounts.x.ai"));
    assert!(response.contains("Access-Control-Allow-Private-Network: true"));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), server.wait_for_callback())
            .await
            .is_err()
    );
    server.cancel();
}

#[tokio::test]
async fn callback_preflight_rejects_non_xai_origin_without_resolving() {
    let mut server = OAuthCallbackServer::new(0);
    server.start().await.unwrap();

    let response = send_request(
        server.config.port,
        "OPTIONS /callback HTTP/1.1\r\nOrigin: https://attacker.example\r\nAccess-Control-Request-Method: GET\r\nAccess-Control-Request-Private-Network: true\r\n\r\n",
    )
    .await
    .unwrap();

    assert!(response.contains("403 Forbidden"));
    assert!(!response.contains("Access-Control-Allow-Origin"));
    assert!(!response.contains("Access-Control-Allow-Private-Network"));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), server.wait_for_callback())
            .await
            .is_err()
    );
    server.cancel();
}

#[tokio::test]
async fn callback_rejects_non_get_methods_without_resolving() {
    let mut server = OAuthCallbackServer::new(0);
    server.start().await.unwrap();

    let response = send_request(
        server.config.port,
        "POST /callback?code=abc HTTP/1.1\r\n\r\n",
    )
    .await
    .unwrap();

    assert!(response.contains("405 Method Not Allowed"));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), server.wait_for_callback())
            .await
            .is_err()
    );
    server.cancel();
}

#[tokio::test]
async fn cancel_unblocks_wait_for_callback() {
    let mut server = OAuthCallbackServer::new(0);
    server.start().await.unwrap();

    server.cancel();
    let error = server.wait_for_callback().await.unwrap_err();

    assert_eq!(error, OAuthError::Cancelled);
}

async fn send_request(port: u16, request: &str) -> Result<String, std::io::Error> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    stream.write_all(request.as_bytes()).await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    Ok(String::from_utf8_lossy(&response).into_owned())
}
