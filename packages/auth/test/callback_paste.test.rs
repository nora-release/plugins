#[tokio::test(flavor = "multi_thread")]
async fn callback_paste_codex_keeps_the_authorization_when_its_port_is_busy() {
    let _port = CODEX_CALLBACK_PORT.lock().await;
    let occupied = tokio::net::TcpListener::bind(("127.0.0.1", 1455))
        .await
        .unwrap();
    let client = Arc::new(StubHttpClient::default());
    client.push(200, serde_json::to_vec(&json!({"access_token":"fixture-access","refresh_token":"fixture-refresh","expires_in":3600})).unwrap());
    let (url, mut urls) = tokio::sync::mpsc::unbounded_channel();
    let (input, mut inputs) = tokio::sync::mpsc::unbounded_channel();
    let callbacks = OAuthLoginCallbacks::new(
        move |value| {
            let _ = url.send(value.to_owned());
        },
        |_| {},
    )
    .with_callback(move |value, blocked| {
        let _ = input.send((value, blocked));
    });
    let mock = client.clone();
    let task =
        tokio::spawn(async move { OAuthLogin::login_openai_codex(callbacks, mock.as_ref()).await });
    let address = tokio::time::timeout(Duration::from_millis(300), urls.recv())
        .await
        .ok()
        .flatten();
    assert!(
        address.is_some(),
        "busy callback port must still publish the same sign-in URL"
    );
    let fields = query_params(&address.unwrap());
    assert_eq!(
        fields["redirect_uri"],
        "http://localhost:1455/auth/callback"
    );
    let (input, blocked) = inputs.recv().await.unwrap();
    assert_eq!(blocked.as_deref(), Some("busy"));
    let callback = format!(
        "{}?code=fixture-code&state={}",
        fields["redirect_uri"], fields["state"]
    );
    for invalid in [
        callback.replace("localhost", "evil.invalid"),
        callback.replace(&fields["state"], "wrong"),
        "fixture-code".into(),
        String::new(),
    ] {
        assert!(input.submit(&invalid).is_err());
    }
    assert!(client.requests().is_empty());
    assert!(!task.is_finished());
    input.submit(&format!("  {callback}\n")).unwrap();
    let credentials = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(credentials.access_token, "fixture-access");
    assert!(input.submit(&callback).is_err());
    let requests = client.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, "https://auth.openai.com/oauth/token");
    let form: BTreeMap<_, _> = url::form_urlencoded::parse(requests[0].body.as_ref().unwrap())
        .into_owned()
        .collect();
    assert_eq!(form["redirect_uri"], fields["redirect_uri"]);
    assert_eq!(form["code"], "fixture-code");
    assert_eq!(
        OAuthPkce::new(&form["code_verifier"]).challenge,
        fields["code_challenge"]
    );
    assert_eq!(occupied.local_addr().unwrap().port(), 1455);
    assert!(tokio::net::TcpStream::connect(("127.0.0.1", 1455))
        .await
        .is_ok());
}

#[tokio::test]
async fn callback_paste_is_opt_in_for_callers_without_an_input_handler() {
    let _port = CODEX_CALLBACK_PORT.lock().await;
    let _occupied = tokio::net::TcpListener::bind(("127.0.0.1", 1455))
        .await
        .unwrap();
    let client = StubHttpClient::default();
    let result = tokio::time::timeout(
        Duration::from_millis(300),
        OAuthLogin::login_openai_codex(
            OAuthLoginCallbacks::new(|_| panic!("no usable callback channel"), |_| {}),
            &client,
        ),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(OAuthError::Transport(_))));
    assert!(client.requests().is_empty());
}
