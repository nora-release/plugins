use super::*;
use crate::tests::{configured, envelope, Client};

async fn released(url: &str) {
    let port = url::Url::parse(url).unwrap().port().unwrap();
    for _ in 0..100 {
        if let Ok(listener) = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await {
            drop(listener);
            return;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("The completed AutoClaw sign-in did not release its callback port");
}
#[tokio::test]
async fn real_listener_form_redirect_keeps_the_callback_alive_and_only_the_issued_state_is_accepted(
) {
    let client = Client::new(vec![
        envelope(json!({"enabled":false})),
        envelope(
            json!({"oauth_url":"https://accounts.google.test/authorize?state=original-state"}),
        ),
        envelope(
            json!({"access_token":"new-access","refresh_token":"new-refresh","user_id":"real-uid"}),
        ),
        envelope(json!({"user_name":"Tester","user_id":"real-uid"})),
    ]);
    let provider = configured(Site::International, client.clone());
    let flow = begin(
        &provider,
        client.clone(),
        "google",
        &[0],
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    let base = text(&flow, "url");
    let local = nora_provider_plugin_sdk::http_client(None).unwrap();
    let page = local.get(base).send().await.unwrap();
    assert_eq!(page.status().as_u16(), 200);
    let page = page.text().await.unwrap();
    assert!(page.contains("method=\"POST\" action=\"/open-auth\""));
    assert!(page.contains("if(!formNavigation)"));
    assert!(!page.contains("oauth-login"));
    let forbidden = local
        .post(format!("{base}abort"))
        .header("origin", "https://evil.invalid")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden.status().as_u16(), 403);
    let redirect = local.post(format!("{base}open-auth")).send().await.unwrap();
    assert_eq!(redirect.status().as_u16(), 303);
    assert_eq!(
        redirect.headers()["location"],
        "https://accounts.google.test/authorize?state=original-state"
    );
    assert_eq!(
        local
            .get(format!("{base}callback?code=evil&state=original-state"))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        404
    );
    assert_eq!(
        local
            .get(format!(
                "{base}auth/callback-google?code=evil&state=other-state"
            ))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        400
    );
    assert_eq!(client.requests.lock().unwrap().len(), 2);
    let good = local
        .get(format!(
            "{base}auth/callback-google?code=accepted&state=original-state"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(good.status().as_u16(), 200);
    let result = provider
        .callback(json!({"session":flow["session"]}))
        .await
        .unwrap();
    assert_eq!(result["credentials"]["name"], "Tester");
    assert_eq!(
        result["credentials"]["accountIdentity"]["subject"],
        "real-uid"
    );
    let requests = client.requests.lock().unwrap();
    let exchange: Value = serde_json::from_slice(requests[2].body.as_ref().unwrap()).unwrap();
    assert_eq!(exchange["code"], "accepted");
    assert_eq!(exchange["state"], "original-state");
    assert!(exchange["navigate_uri"]
        .as_str()
        .unwrap()
        .starts_with("http://localhost:"));
    drop(requests);
    released(base).await;
}

#[tokio::test]
async fn browser_closing_before_authorization_and_host_cancel_both_release_their_own_session() {
    for browser in [false, true] {
        let client = Client::new(vec![]);
        let provider = configured(Site::International, client.clone());
        let flow = begin(
            &provider,
            client.clone(),
            "zai",
            &[0],
            Duration::from_secs(10),
        )
        .await
        .unwrap();
        let base = text(&flow, "url");
        if browser {
            let local = nora_provider_plugin_sdk::http_client(None).unwrap();
            assert_eq!(
                local
                    .post(format!("{base}abort"))
                    .header("content-type", "application/json")
                    .body("{}")
                    .send()
                    .await
                    .unwrap()
                    .status()
                    .as_u16(),
                200
            );
            assert!(provider
                .callback(json!({"session":flow["session"]}))
                .await
                .is_err());
        } else {
            assert_eq!(
                auth::cancel(&provider, &json!({"session":flow["session"]}))
                    .await
                    .unwrap()["cancelled"],
                true
            );
        }
        released(base).await;
        assert!(client.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn a_missing_state_or_bad_authorization_scheme_never_redirects_or_consumes_a_code() {
    for url in [
        "https://accounts.google.test/authorize",
        "javascript:alert(1)?state=attacker",
        "https://user:password@accounts.google.test/authorize?state=attacker",
    ] {
        let client = Client::new(vec![envelope(json!({"oauth_url":url}))]);
        let provider = configured(Site::International, client.clone());
        let flow = begin(
            &provider,
            client.clone(),
            "google",
            &[0],
            Duration::from_secs(10),
        )
        .await
        .unwrap();
        let base = text(&flow, "url");
        let local = nora_provider_plugin_sdk::http_client(None).unwrap();
        let reply = local.post(format!("{base}open-auth")).send().await.unwrap();
        assert_eq!(reply.status().as_u16(), 502);
        assert!(reply.headers().get("location").is_none());
        assert_eq!(
            local
                .get(format!(
                    "{base}auth/callback-google?code=evil&state=attacker"
                ))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16(),
            400
        );
        auth::cancel(&provider, &json!({"session":flow["session"]}))
            .await
            .unwrap();
        released(base).await;
        assert_eq!(client.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn listeners_are_ready_before_return_fall_back_over_busy_ports_and_expire_without_callback() {
    let busy = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let port = busy.local_addr().unwrap().port();
    let client = Client::new(vec![]);
    let provider = configured(Site::International, client.clone());
    assert!(begin(
        &provider,
        client.clone(),
        "google",
        &[port],
        Duration::from_secs(1)
    )
    .await
    .is_err());
    let flow = begin(
        &provider,
        client.clone(),
        "google",
        &[port, 0],
        Duration::from_millis(10),
    )
    .await
    .unwrap();
    let base = text(&flow, "url");
    assert_ne!(url::Url::parse(base).unwrap().port(), Some(port));
    let socket = TcpStream::connect((
        std::net::Ipv4Addr::LOCALHOST,
        url::Url::parse(base).unwrap().port().unwrap(),
    ))
    .await
    .unwrap();
    drop(socket);
    tokio::time::sleep(Duration::from_millis(30)).await;
    released(base).await;
    assert!(provider
        .callback(json!({"session":flow["session"]}))
        .await
        .is_err());
    assert!(client.requests.lock().unwrap().is_empty());
}

#[test]
fn captcha_page_only_bridges_the_official_sdk_to_the_native_authorization_form() {
    let page = html(
        "zai",
        &json!({"enabled":true,"scene_id":"real-scene","region":"ga","prefix":"sq51tr"}),
    );
    assert!(page.contains("https://o.alicdn.com/captcha-frontend/aliyunCaptcha/AliyunCaptcha.js"));
    assert!(page.contains("name=\"receipt\""));
    assert!(page.contains("Continue authorization"));
    assert!(page.contains("goBtn\" disabled"));
    assert!(!page.contains("access_token"));
    assert!(!page.contains("fetch("));
    assert!(!page.contains("/oauth-url"));
    let unsafe_config = html(
        "google",
        &json!({"enabled":true,"scene_id":"</script><script>alert(1)</script>"}),
    );
    assert!(!unsafe_config.contains("alert(1)"));
}

#[tokio::test]
async fn captcha_receipt_stays_in_the_native_form_exchange_and_zai_uses_its_registered_callback() {
    let client = Client::new(vec![
        envelope(json!({"enabled":true,"scene_id":"scene","region":"ga","prefix":"sq51tr"})),
        envelope(json!({"oauth_url":"https://zai.test/authorize?state=zai-issued"})),
    ]);
    let provider = configured(Site::International, client.clone());
    let flow = begin(
        &provider,
        client.clone(),
        "zai",
        &[0],
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    let base = text(&flow, "url");
    let local = nora_provider_plugin_sdk::http_client(None).unwrap();
    let page = local.get(base).send().await.unwrap().text().await.unwrap();
    assert!(page.contains("AliyunCaptcha.js"));
    let response = local
        .post(format!("{base}open-auth"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body("receipt=%7B%22verified%22%3Atrue%7D")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 303);
    let requests = client.requests.lock().unwrap();
    let body: Value = serde_json::from_slice(requests[1].body.as_ref().unwrap()).unwrap();
    assert_eq!(body["ali_captcha_verify_param"], "{\"verified\":true}");
    assert!(body["navigate_uri"]
        .as_str()
        .unwrap()
        .ends_with("/auth/callback-zai"));
    assert!(requests[1]
        .url
        .ends_with("/userapi/overseasv1/zai-oauth-url"));
    drop(requests);
    auth::cancel(&provider, &json!({"session":flow["session"]}))
        .await
        .unwrap();
    released(base).await;
}
