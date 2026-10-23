use super::*;
use std::collections::BTreeMap;
#[path = "fixture.rs"]
mod fixture;
async fn begin(replies: Vec<fixture::Reply>) -> (Pending, url::Url, Arc<fixture::Client>) {
    let client = Arc::new(fixture::Client::new(replies));
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let (pending, address) =
        start_listener(client.clone(), listener, "fixture-installation").unwrap();
    (pending, url::Url::parse(&address).unwrap(), client)
}
async fn browse(port: u16, path: &str) -> String {
    let mut socket = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    socket
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut bytes = Vec::new();
    socket.read_to_end(&mut bytes).await.unwrap();
    String::from_utf8(bytes).unwrap()
}
fn port(url: &url::Url) -> u16 {
    url::Url::parse(
        &url.query_pairs()
            .find(|(key, _)| key == "redirect_uri")
            .unwrap()
            .1,
    )
    .unwrap()
    .port()
    .unwrap()
}
#[tokio::test]
async fn actual_social_callback_checks_state_and_uses_pkce_before_returning_credentials() {
    let(pending,url,client)=begin(vec![fixture::Reply::json(json!({"accessToken":"access","refreshToken":"refresh","expiresIn":3600,"profileArn":fixture::PROFILE})),fixture::Reply::json(fixture::usage())]).await;
    let query = url.query_pairs().collect::<BTreeMap<_, _>>();
    assert_eq!(query["code_challenge_method"], "S256");
    let bad = browse(
        port(&url),
        "/oauth/callback?state=wrong&code=private-code&login_option=google",
    )
    .await;
    assert!(bad.starts_with("HTTP/1.1 400"));
    assert!(client.requests.lock().unwrap().is_empty());
    let response = browse(
        port(&url),
        &format!(
            "/signin/callback?state={}&code=private-code&login_option=google",
            query["state"]
        ),
    )
    .await;
    assert!(!response.contains("private-code"));
    let credential = pending.complete().await.unwrap();
    assert_eq!(credential.email, "person@example.test");
    assert_eq!(credential.method, "social");
    assert!(credential.realm.is_empty());
    let requests = client.requests.lock().unwrap();
    let body: Value = serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(
        requests[0].url,
        "https://prod.us-east-1.auth.desktop.kiro.dev/oauth/token"
    );
    assert_eq!(body["code"], "private-code");
    assert_eq!(
        challenge(body["code_verifier"].as_str().unwrap()),
        query["code_challenge"]
    );
    assert!(body["redirect_uri"]
        .as_str()
        .unwrap()
        .ends_with("/signin/callback?login_option=google"));
    assert!(requests[0].headers["user-agent"].starts_with("KiroIDE-1.1.70-"));
}
#[tokio::test]
async fn builder_callback_registers_an_aws_client_and_checks_a_distinct_second_state() {
    let (pending, url, client) = begin(vec![
        fixture::Reply::json(json!({"clientId":"aws-client","clientSecret":"aws-secret"})),
        fixture::Reply::json(
            json!({"accessToken":"access","refreshToken":"refresh","expiresIn":3600}),
        ),
        fixture::Reply::json(fixture::usage()),
    ])
    .await;
    let query = url.query_pairs().collect::<BTreeMap<_, _>>();
    let response=browse(port(&url),&format!("/oauth/callback?state={}&login_option=builderid&idc_region=eu-west-1&issuer_url=https%3A%2F%2Fview.awsapps.com%2Fstart",query["state"])).await;
    assert!(response.starts_with("HTTP/1.1 302"));
    assert!(!response.contains("aws-secret"));
    let location = response
        .lines()
        .find_map(|line| line.strip_prefix("Location: "))
        .unwrap();
    let authorize = url::Url::parse(location).unwrap();
    assert_eq!(authorize.host_str(), Some("oidc.eu-west-1.amazonaws.com"));
    let aws = authorize.query_pairs().collect::<BTreeMap<_, _>>();
    assert_ne!(aws["state"], query["state"]);
    let response = browse(
        port(&url),
        &format!("/oauth/callback?state={}&code=aws-code", aws["state"]),
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 200"));
    let credential = pending.complete().await.unwrap();
    assert_eq!(credential.method, "idc");
    assert!(credential.builder);
    assert_eq!(credential.client_secret, "aws-secret");
    assert_eq!(credential.profile, session::BUILDER_PROFILE);
    assert!(credential.realm.is_empty());
    let requests = client.requests.lock().unwrap();
    let registration: Value = serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(
        registration["redirectUris"],
        json!(["http://127.0.0.1/oauth/callback"])
    );
    assert_eq!(registration["scopes"].as_array().unwrap().len(), 5);
    let exchange: Value = serde_json::from_slice(requests[1].body.as_ref().unwrap()).unwrap();
    assert_eq!(exchange["clientSecret"], "aws-secret");
    assert_eq!(exchange["code"], "aws-code");
    assert_eq!(
        challenge(exchange["codeVerifier"].as_str().unwrap()),
        aws["code_challenge"]
    );
}
#[tokio::test]
async fn invalid_state_errors_do_not_finish_another_login_and_rejections_are_redacted() {
    let (pending, url, client) = begin(vec![]).await;
    let query = url.query_pairs().collect::<BTreeMap<_, _>>();
    let response = browse(
        port(&url),
        "/oauth/callback?state=other&error=private-token",
    )
    .await;
    assert!(!response.contains("private-token"));
    let response = browse(
        port(&url),
        &format!(
            "/oauth/callback?state={}&error=private-token&error_description=private-secret",
            query["state"]
        ),
    )
    .await;
    assert!(!response.contains("private-secret"));
    let error = pending.complete().await.err().unwrap();
    assert!(!error.message.contains("private"));
    assert!(client.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn company_browser_flow_is_explicitly_unsupported_without_sending_its_code() {
    let (pending, url, client) = begin(vec![]).await;
    let state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    browse(
        port(&url),
        &format!("/oauth/callback?state={state}&login_option=external_idp&code=private"),
    )
    .await;
    assert_eq!(
        pending.complete().await.err().unwrap().code,
        "method_not_supported"
    );
    assert!(client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn enterprise_browser_callback_preserves_the_same_realm_as_cli_and_ide() {
    let (pending, url, client) = begin(vec![
        fixture::Reply::json(json!({"clientId":"aws-client","clientSecret":"aws-secret"})),
        fixture::Reply::json(json!({"accessToken":"private-access","refreshToken":"private-refresh","expiresIn":3600,"profileArn":fixture::PROFILE})),
        fixture::Reply::json(json!({"userInfo":{"userId":"verified-user","email":"person@example.test"}})),
    ]).await;
    let query = url.query_pairs().collect::<BTreeMap<_, _>>();
    let response = browse(port(&url), &format!("/oauth/callback?state={}&login_option=awsidc&idc_region=eu-central-1&issuer_url=https%3A%2F%2Ftenant.awsapps.com%2Fstart%2F", query["state"])).await;
    assert!(response.starts_with("HTTP/1.1 302"));
    let authorize = url::Url::parse(
        response
            .lines()
            .find_map(|line| line.strip_prefix("Location: "))
            .unwrap(),
    )
    .unwrap();
    let next = authorize.query_pairs().collect::<BTreeMap<_, _>>();
    let response = browse(
        port(&url),
        &format!("/oauth/callback?state={}&code=private-code", next["state"]),
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(!response.contains("private"));
    let credential = pending.complete().await.unwrap();
    assert!(!credential.builder);
    assert_eq!(credential.realm, "https://tenant.awsapps.com/start");
    let mut cli = credential.clone();
    cli.realm = "https://tenant.awsapps.com/start/".into();
    assert_eq!(cli.account_identity(), credential.account_identity());
    assert_eq!(
        credential.saved()["realm"],
        "https://tenant.awsapps.com/start"
    );
    let request = client.requests.lock().unwrap();
    let registration: Value = serde_json::from_slice(request[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(
        registration["issuerUrl"],
        "https://tenant.awsapps.com/start/"
    );
}
