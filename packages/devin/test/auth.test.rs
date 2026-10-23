use super::*;
use crate::fixture::{self, Client, Reply};
#[tokio::test]
async fn browser_pkce_callback_uses_its_own_state_and_actual_key_identity() {
    let client = Client::new(vec![
        Reply::json(json!({"sessionToken":"private-browser-key"})),
        Reply::json(fixture::me()),
    ]);
    let (pending, url) = start(client.clone()).await.unwrap();
    let url = url::Url::parse(&url).unwrap();
    assert_eq!(url.origin().ascii_serialization(), "https://app.devin.ai");
    let query = url
        .query_pairs()
        .into_owned()
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(query["code_challenge_method"], "S256");
    let redirect = url::Url::parse(&query["redirect_uri"]).unwrap();
    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", redirect.port().unwrap()))
        .await
        .unwrap();
    socket
        .write_all(
            format!(
                "GET /callback?state={}&code=private-code HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
                query["state"]
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let account = pending.complete().await.unwrap();
    assert_eq!(account.email, "one@example.test");
    assert_eq!(account.key, "private-browser-key");
    let mut reply = String::new();
    socket.read_to_string(&mut reply).await.unwrap();
    assert!(!reply.contains("private"));
    let requests = client.requests.lock().unwrap();
    let exchange: Value = serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(exchange["code"], "private-code");
    let verifier = exchange["code_verifier"].as_str().unwrap();
    assert_eq!(
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier)),
        query["code_challenge"]
    );
    assert!(requests[1].url.ends_with("/GetUserStatus"));
}
#[tokio::test]
async fn dropping_pending_sign_in_closes_only_its_listener() {
    let client = Client::new(vec![]);
    let (one, url) = start(client.clone()).await.unwrap();
    let (two, other) = start(client).await.unwrap();
    let redirect = |url: &str| {
        let url = url::Url::parse(url).unwrap();
        let pairs = url
            .query_pairs()
            .collect::<std::collections::BTreeMap<_, _>>();
        url::Url::parse(&pairs["redirect_uri"])
            .unwrap()
            .port()
            .unwrap()
    };
    let port = redirect(&url);
    let other = redirect(&other);
    drop(one);
    tokio::task::yield_now().await;
    assert!(tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_err());
    assert!(tokio::net::TcpStream::connect(("127.0.0.1", other))
        .await
        .is_ok());
    drop(two);
}
#[tokio::test]
async fn unrecognized_account_identity_is_not_replaced_with_placeholder_success() {
    let client = Client::new(vec![Reply::json(json!({"userStatus":{}}))]);
    assert!(finish(
        client.as_ref(),
        "private-key".into(),
        network::SERVER.into(),
        None
    )
    .await
    .is_err());
}
