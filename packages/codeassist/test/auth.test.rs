use super::*;
use std::collections::BTreeMap;
#[path = "fixture.rs"]
mod fixture;
#[tokio::test]
async fn refresh_uses_the_correct_oauth_client_and_retains_an_unrotated_refresh_token() {
    let client = fixture::Client::new(vec![fixture::Reply::json(
        json!({"access_token":"new-access","expires_in":3600}),
    )]);
    let previous = session::Tokens {
        refresh: "private-refresh".into(),
        ..Default::default()
    };
    let reply = exchange(
        App::Antigravity,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &previous.refresh),
        ],
        &client,
    )
    .await
    .unwrap();
    let next = read_tokens(&reply, Some(&previous)).unwrap();
    assert_eq!(next.refresh, "private-refresh");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].url, TOKEN);
    let form =
        url::form_urlencoded::parse(requests[0].body.as_ref().unwrap()).collect::<BTreeMap<_, _>>();
    assert_eq!(form.get("client_id").unwrap(), App::Antigravity.client_id());
    assert!(form
        .get("refresh_token")
        .is_some_and(|value| value == "private-refresh"));
}
#[tokio::test]
async fn browser_signins_use_distinct_clients_scopes_redirects_and_pkce() {
    for app in [App::GeminiCLI, App::Antigravity] {
        let client = std::sync::Arc::new(fixture::Client::new(vec![]));
        let (mut pending, address) = start(app, String::new(), client).await.unwrap();
        let url = url::Url::parse(&address).unwrap();
        let query = url.query_pairs().collect::<BTreeMap<_, _>>();
        assert_eq!(query["client_id"], app.client_id());
        assert_eq!(query["code_challenge_method"], "S256");
        assert_ne!(query["code_challenge"], pending.pkce.verifier);
        assert!(query["scope"].contains("cloud-platform"));
        assert_eq!(
            query["scope"].contains("experimentsandconfigs"),
            app == App::Antigravity
        );
        assert!(pending.redirect.ends_with(if app == App::Antigravity {
            "/oauth-callback"
        } else {
            "/oauth2callback"
        }));
        pending.server.stop().await;
    }
}
#[tokio::test]
async fn refusals_and_invalid_responses_do_not_return_sensitive_contents() {
    for reply in [
        fixture::Reply::raw(401, b"private-token"),
        fixture::Reply::raw(500, b"private-token"),
        fixture::Reply::raw(200, b"private-token"),
    ] {
        let client = fixture::Client::new(vec![reply]);
        let error = json_request(&client, HttpRequest::new(HttpMethod::Get, USER_INFO))
            .await
            .unwrap_err();
        assert!(!error.message.contains("private-token"));
    }
    let client = fixture::Client::new(vec![fixture::Reply::raw(
        200,
        &vec![b' '; 4 * 1024 * 1024 + 1],
    )]);
    assert!(
        json_request(&client, HttpRequest::new(HttpMethod::Get, USER_INFO))
            .await
            .unwrap_err()
            .message
            .contains("large")
    );
}
#[test]
fn incomplete_or_invalid_token_pairs_are_refused() {
    assert!(read_tokens(&json!({"access_token":"private","expires_in":3600}), None).is_err());
    assert!(read_tokens(
        &json!({"access_token":"private","refresh_token":"refresh","expires_in":-1}),
        None
    )
    .is_err());
}

#[tokio::test]
async fn actual_loopback_callback_exchanges_the_code_once_and_checks_state_before_http() {
    use tokio::io::AsyncWriteExt;
    for valid in [true, false] {
        let client = std::sync::Arc::new(fixture::Client::new(if valid {
            vec![
                fixture::Reply::json(
                    json!({"access_token":"private-access","refresh_token":"private-refresh","expires_in":3600}),
                ),
                fixture::Reply::json(
                    json!({"id":"person-a","email":"person@example.test","verified_email":true}),
                ),
            ]
        } else {
            vec![]
        }));
        let (pending, _) = start(App::GeminiCLI, "project-a".into(), client.clone())
            .await
            .unwrap();
        let redirect = url::Url::parse(&pending.redirect).unwrap();
        let state = if valid {
            pending.state.clone()
        } else {
            "other-state".into()
        };
        let mut browser = tokio::net::TcpStream::connect(("127.0.0.1", redirect.port().unwrap()))
            .await
            .unwrap();
        browser.write_all(format!("GET {}?code=fixture-code&state={} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",redirect.path(),state).as_bytes()).await.unwrap();
        let result = complete(pending, App::GeminiCLI).await;
        if valid {
            let tokens = result.ok().unwrap();
            assert_eq!(tokens.email, "person@example.test");
            assert_eq!(tokens.project, "project-a");
            assert_eq!(client.requests.lock().unwrap().len(), 2);
        } else {
            assert!(result.is_err());
            assert!(client.requests.lock().unwrap().is_empty());
        }
    }
}
