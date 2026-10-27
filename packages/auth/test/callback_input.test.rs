use super::*;

#[tokio::test]
async fn callback_paste_validates_loopback_port_path_state_and_bounds_without_consuming() {
    for path in ["/auth/callback", "/oauth2callback", "/oauth-callback"] {
        let mut server = OAuthCallbackServer::with_path(0, path);
        server.expect_state("matching");
        server.start().await.unwrap();
        let input = server.callback_input().unwrap();
        let good = format!("{}?code=fixture&state=matching", server.redirect_uri());
        let invalid = [
            good.replace("127.0.0.1", "evil.invalid"),
            good.replace("http:", "https:"),
            good.replace(&format!(":{}", server.config.port), ":1"),
            good.replace(path, "/elsewhere"),
            good.replace("matching", "other"),
            good.replace("code=fixture", "code="),
            good.replace("code=fixture", "missing=fixture"),
            format!("{good}&state=matching"),
            format!("{good}#fragment"),
            good.replace("127.0.0.1", "user@127.0.0.1"),
            good.replace("fixture", &"a".repeat(8193)),
            "fixture".into(),
            String::new(),
            "a".repeat(16385),
        ];
        for address in invalid {
            assert!(input.submit(&address).is_err(), "{path}");
        }
        input.submit(&format!("  {good}\n")).unwrap();
        assert_eq!(server.wait_for_callback().await.unwrap()["code"], "fixture");
        input.submit(&good).unwrap();
        drop(server);
        assert!(input.submit(&good).is_err());
    }
}

#[tokio::test]
async fn callback_paste_and_browser_claim_only_one_result_and_bad_browser_state_keeps_waiting() {
    for host in ["localhost", "127.0.0.1", "[::1]"] {
        let mut server = OAuthCallbackServer::with_path(0, "/auth/callback");
        server.expect_state("matching");
        server.start().await.unwrap();
        let port = server.config.port;
        let input = server.callback_input().unwrap();
        let bad = callback_http(port, "/auth/callback?code=bad&state=other").await;
        assert!(bad.contains("400 Bad Request"));
        let good = format!("http://{host}:{port}/auth/callback?code=fixture&state=matching");
        let (pasted, browser) = tokio::join!(
            async { input.submit(&good) },
            callback_http(port, "/auth/callback?code=fixture&state=matching")
        );
        assert!(browser.contains("200 OK"));
        assert!(pasted.is_ok() || browser.contains("Login complete"));
        assert_eq!(server.wait_for_callback().await.unwrap()["code"], "fixture");
        input.submit(&good).unwrap();
        drop(server);
        assert!(input.submit(&good).is_err());
    }
}

#[tokio::test]
async fn callback_paste_cancellation_closes_listener_and_incomplete_connections() {
    let mut server = OAuthCallbackServer::new(0);
    server.expect_state("matching");
    server.start().await.unwrap();
    let input = server.callback_input().unwrap();
    let port = server.config.port;
    let mut pending = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    pending.write_all(b"GET /callback").await.unwrap();
    tokio::task::yield_now().await;
    server.cancel();
    assert_eq!(
        server.wait_for_callback().await.unwrap_err(),
        OAuthError::Cancelled
    );
    assert!(input
        .submit(&format!(
            "http://localhost:{port}/callback?code=fixture&state=matching"
        ))
        .is_err());
    let mut byte = [0; 1];
    assert!(
        tokio::time::timeout(Duration::from_secs(1), pending.read(&mut byte))
            .await
            .is_ok()
    );
    assert!(tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .is_ok());
}

async fn callback_http(port: u16, path: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut body = Vec::new();
    stream.read_to_end(&mut body).await.unwrap();
    String::from_utf8(body).unwrap()
}
