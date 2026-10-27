#[tokio::test]
async fn callback_paste_chatgpt_offers_auto_and_paste_together() {
    let directory = Directory::new();
    let client = Client::new(vec![]);
    let (start, flow) = authorize(&endpoints(), &directory.0, client.clone())
        .await
        .unwrap();
    assert_eq!(start["method"], "auto");
    assert_eq!(start["pasteCallback"], true);
    assert_eq!(start["callbackMode"], "poll");
    flow.cancel();
    assert!(client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn callback_paste_chatgpt_reuses_registration_and_auto_callback_can_race_once() {
    let directory = Directory::new();
    let client = Client::new(vec![]);
    let (start, flow) = authorize(&endpoints(), &directory.0, client.clone())
        .await
        .unwrap();
    let query: BTreeMap<_, _> = url::Url::parse(start["url"].as_str().unwrap())
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect();
    let mut token = response("access-one", "refresh-one");
    token["id_token"] = json!(jwt(
        json!({"iss":ISSUER,"aud":"oaiapp_registered","nonce":query["nonce"],"sub":"member","email":"person@example.test","exp":now()/1000+3600})
    ));
    let mut reply = Reply::json(200, token);
    reply.delay = Duration::from_millis(80);
    client.replies.lock().unwrap().push_back(reply);
    let good = format!(
        "{}?state={}&code=fixture&client_id=oaiapp_registered",
        query["redirect_uri"], query["state"]
    );
    for invalid in [
        good.replace(&query["state"], "wrong"),
        good.replace("127.0.0.1", "evil.invalid"),
        "fixture".into(),
    ] {
        let reply = poll(
            &flow,
            "session",
            &json!({"poll":true,"callbackURL":invalid}),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(reply["inputError"].is_string());
    }
    assert!(client.requests.lock().unwrap().is_empty());
    let accepted = poll(&flow, "session", &json!({"poll":true,"callbackURL":good}))
        .await
        .unwrap();
    assert!(accepted.is_some());
    let browser = visit(
        &query["redirect_uri"],
        &[
            ("state", &query["state"]),
            ("code", "fixture"),
            ("client_id", "oaiapp_registered"),
        ],
    )
    .await;
    assert!(browser.contains("already finishing"));
    let duplicate = poll(&flow, "session", &json!({"poll":true,"callbackURL":good}))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(duplicate["pending"], true);
    assert!(
        duplicate.get("inputError").is_none(),
        "a claimed callback must wait for the same exchange"
    );
    let saved = callback(&flow).await.unwrap();
    assert_eq!(saved["email"], "person@example.test");
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    let form = client.form(0);
    assert_eq!(form["redirect_uri"], query["redirect_uri"]);
    assert_eq!(form["code"], "fixture");
    assert_eq!(
        URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes())),
        query["code_challenge"]
    );
    assert!(callback(&flow).await.is_err());
}

#[tokio::test]
async fn callback_paste_chatgpt_port_fallback_and_cancellation_keep_the_other_service_alive() {
    let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut endpoints = endpoints();
    endpoints.callback = occupied.local_addr().unwrap().to_string();
    let directory = Directory::new();
    let client = Client::new(vec![]);
    let (start, flow) = authorize(&endpoints, &directory.0, client.clone())
        .await
        .unwrap();
    let query: BTreeMap<_, _> = url::Url::parse(start["url"].as_str().unwrap())
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect();
    let redirect = url::Url::parse(&query["redirect_uri"]).unwrap();
    assert_ne!(
        redirect.port().unwrap(),
        occupied.local_addr().unwrap().port()
    );
    let input = flow.result.lock().await.callback.clone().unwrap();
    flow.cancel();
    assert!(callback(&flow).await.is_err());
    tokio::task::yield_now().await;
    assert!(input
        .submit(&format!(
            "{}?state={}&code=fixture&client_id=oaiapp_registered",
            query["redirect_uri"], query["state"]
        ))
        .is_err());
    assert!(
        tokio::net::TcpListener::bind(("127.0.0.1", redirect.port().unwrap()))
            .await
            .is_ok()
    );
    assert!(
        tokio::net::TcpStream::connect(occupied.local_addr().unwrap())
            .await
            .is_ok()
    );
    assert!(client.requests.lock().unwrap().is_empty());
}
