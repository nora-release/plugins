use super::*;
use crate::provider::tests::{configured, credentials, Client};

fn token() -> Value {
    json!({"credentials":{"access_key_id":"new-AK","secret_access_key":"new-SK","security_token":"new-ST","expiration":"2099-01-01T00:00:00Z"},"refresh_token":"new-refresh"})
}

#[tokio::test]
async fn browser_callback_uses_pkce_dpop_and_the_real_sts_form() {
    let client = Client::new(vec![(200, token())]);
    let provider = configured(client.clone());
    let flow = authorize(&provider, json!({"method":"oauth"}))
        .await
        .unwrap();
    let url = url::Url::parse(flow["url"].as_str().unwrap()).unwrap();
    let pairs = url.query_pairs().collect::<BTreeMap<_, _>>();
    assert_eq!(url.host_str(), Some("codearts.huaweicloud.com"));
    assert_eq!(pairs.get("code_challenge_method").unwrap(), "SHA-256");
    let port = pairs["port"].parse::<u16>().unwrap();
    assert!(port >= 10000);
    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    socket
        .write_all(b"GET /oauth/callback?code=browser-code HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut redirect = Vec::new();
    socket.read_to_end(&mut redirect).await.unwrap();
    assert!(String::from_utf8(redirect)
        .unwrap()
        .contains("login_succeed=true"));
    let result = callback(&provider, json!({"session":flow["session"]}))
        .await
        .unwrap();
    let saved = decode(&result["credentials"]).unwrap();
    assert_eq!(saved["access_key_id"], "new-AK");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, STS);
    assert!(requests[0].headers.contains_key("dpop"));
    let form =
        url::form_urlencoded::parse(requests[0].body.as_ref().unwrap()).collect::<BTreeMap<_, _>>();
    assert_eq!(form["code"], "browser-code");
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(
        form["code_verifier"],
        saved["code_verifier"].as_str().unwrap()
    );
    assert_eq!(
        form["redirect_uri"],
        format!("http://127.0.0.1:{port}/oauth/callback")
    );
}

#[tokio::test]
async fn refresh_is_single_flight_and_preserves_the_original_account_session() {
    let client = Client::new(vec![(200, token())]);
    let provider = configured(client.clone());
    let old = credentials();
    let (a, b) = tokio::join!(
        renew(&provider, client.as_ref(), &old),
        renew(&provider, client.as_ref(), &old)
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a, b);
    assert_eq!(a["session_id"], old["session_id"]);
    assert_eq!(a["refresh_token"], "new-refresh");
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn spent_refresh_is_permanent_but_a_rejected_dpop_proof_is_retryable() {
    for (code, expected) in [
        ("STS5.1806", "authentication_required"),
        ("STS.0001", "upstream_transport_error"),
    ] {
        let client = Client::new(vec![(
            400,
            json!({"error_code":code,"error_msg":"request declined"}),
        )]);
        let provider = configured(client.clone());
        let error = renew(&provider, client.as_ref(), &credentials())
            .await
            .unwrap_err();
        assert_eq!(error.code, expected);
    }
}

#[tokio::test]
async fn rotated_sts_credentials_are_not_returned_until_the_host_confirms_the_save() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let client = Client::new(vec![(200, token())]);
    let provider = Arc::new(configured(client.clone()));
    let (host, plugin) = tokio::io::duplex(64 * 1024);
    let (input, output) = tokio::io::split(plugin);
    let task = tokio::spawn(nora_provider_plugin_sdk::serve_io(provider, input, output));
    let (input, mut output) = tokio::io::split(host);
    let mut input = BufReader::new(input);
    let mut line = String::new();
    let manifest: Value = serde_json::from_str(include_str!("../nora-provider.json")).unwrap();
    output.write_all(format!("{}\n",json!({"id":1,"method":"initialize","params":{"apiVersion":1,"pluginID":"codearts","pluginDirectory":"/test/plugin","dataDirectory":"/test/data","manifest":manifest,"hostCapabilities":{"credentialUpdates":true}}})).as_bytes()).await.unwrap();
    input.read_line(&mut line).await.unwrap();
    assert!(serde_json::from_str::<Value>(&line)
        .unwrap()
        .get("result")
        .is_some());
    line.clear();
    let previous = encode(credentials()).unwrap();
    output
        .write_all(
            format!(
                "{}\n",
                json!({"id":2,"method":"refresh","params":{"credentials":previous}})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    input.read_line(&mut line).await.unwrap();
    let event: Value = serde_json::from_str(&line).unwrap();
    line.clear();
    assert_eq!(event["event"], "credentials");
    assert_eq!(event["credentials"]["refresh"], "new-refresh");
    assert!(
        tokio::time::timeout(Duration::from_millis(25), input.read_line(&mut line))
            .await
            .is_err()
    );
    output.write_all(format!("{}\n",json!({"id":0,"method":"credentialsSaved","params":{"id":2,"updateID":event["updateID"],"success":true}})).as_bytes()).await.unwrap();
    input.read_line(&mut line).await.unwrap();
    let result: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        decode(&result["result"]["credentials"]).unwrap()["security_token"],
        "new-ST"
    );
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    drop(output);
    drop(input);
    task.abort();
}
