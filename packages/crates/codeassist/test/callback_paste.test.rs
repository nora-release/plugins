#[tokio::test]
async fn callback_paste_google_signins_offer_auto_and_paste_together() {
    for app in [App::GeminiCLI, App::Antigravity] {
        let (provider, client) = provider(app, vec![]).await;
        let started = provider
            .authorize(json!({"authMethodID":"oauth"}))
            .await
            .unwrap();
        assert_eq!(started["method"], "auto");
        assert_eq!(started["pasteCallback"], true, "{}", app.id());
        assert_eq!(started["callbackMode"], "poll");
        assert!(client.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn callback_paste_google_rpc_keeps_invalid_input_and_completes_one_exchange() {
    for app in [App::GeminiCLI, App::Antigravity] {
        for browser_first in [false, true] {
            let (provider, client) = provider(app, vec![
                fixture::Reply::json(json!({"access_token":"private-access","refresh_token":"private-refresh","expires_in":3600})),
                fixture::Reply::json(json!({"id":"person-a","email":"person@example.test","verified_email":true})),
                project(),
            ]).await;
            let start = provider
                .authorize(json!({"authMethodID":"oauth","inputs":{"project":"project-a"}}))
                .await
                .unwrap();
            let session = start["session"].clone();
            let query: BTreeMap<_, _> = url::Url::parse(start["url"].as_str().unwrap())
                .unwrap()
                .query_pairs()
                .into_owned()
                .collect();
            let callback = format!(
                "{}?state={}&code=fixture",
                query["redirect_uri"], query["state"]
            );
            let waiting = provider
                .callback(json!({"session":session,"poll":true}))
                .await
                .unwrap();
            assert_eq!(waiting["pending"], true);
            let rejected = provider.callback(json!({"session":session,"poll":true,"callbackURL":callback.replace(&query["state"], "wrong")})).await.unwrap();
            assert!(rejected["inputError"].is_string());
            assert!(client.requests.lock().unwrap().is_empty());
            if browser_first {
                google_callback_visit(&callback).await;
            }
            let mut reply = provider
                .callback(json!({"session":session,"poll":true,"callbackURL":callback}))
                .await
                .unwrap();
            for _ in 0..100 {
                if reply.get("credentials").is_some() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                reply = provider
                    .callback(json!({"session":session,"poll":true}))
                    .await
                    .unwrap();
            }
            assert_eq!(reply["credentials"]["email"], "person@example.test");
            assert_eq!(
                client
                    .requests
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|request| request.url == TOKEN)
                    .count(),
                1
            );
            assert!(provider
                .callback(json!({"session":session,"poll":true}))
                .await
                .is_err());
        }
    }
}

async fn google_callback_visit(raw: &str) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let url = url::Url::parse(raw).unwrap();
    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", url.port().unwrap()))
        .await
        .unwrap();
    socket
        .write_all(
            format!(
                "GET {}?{} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
                url.path(),
                url.query().unwrap()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut reply = Vec::new();
    socket.read_to_end(&mut reply).await.unwrap();
    assert!(String::from_utf8(reply).unwrap().contains("200 OK"));
}
