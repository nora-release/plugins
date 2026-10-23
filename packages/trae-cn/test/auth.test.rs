use super::*;
use crate::tests::{configured, credentials, Client};
fn token(access: &str, refresh: &str) -> Value {
    json!({"Result":{"Token":access,"RefreshToken":refresh,"TokenExpireAt":"2099-01-01T00:00:00Z"}})
}
#[test]
fn device_proof_is_the_clients_verifiable_der_p256_signature() {
    use p256::{
        ecdsa::{signature::Verifier, VerifyingKey},
        pkcs8::DecodePublicKey,
    };
    let (public, private) = keys().unwrap();
    let proof = proof(
        "/trae/api/v3/oauth/ExchangeToken",
        "client",
        "refresh",
        &private,
        1700000000,
        "nonce",
    )
    .unwrap();
    let signature = Signature::from_der(
        &STANDARD
            .decode(proof["Signature"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    VerifyingKey::from_public_key_pem(&public)
        .unwrap()
        .verify(
            b"POST\n/trae/api/v3/oauth/ExchangeToken\nclient\nrefresh\n1700000000\nnonce",
            &signature,
        )
        .unwrap();
}
#[tokio::test]
async fn browser_callbacks_keep_each_realms_account_host_and_own_persistent_device() {
    for site in [Site::China, Site::Global] {
        let client = Client::new(if site == Site::China {
            vec![(200, token("device-bound", "same-refresh"))]
        } else {
            vec![]
        });
        let provider = configured(site, client.clone());
        let flow = authorize(&provider, json!({"authMethodID":"oauth"}))
            .await
            .unwrap();
        let url = url::Url::parse(flow["url"].as_str().unwrap()).unwrap();
        assert_eq!(
            url.host_str(),
            url::Url::parse(site.web()).unwrap().host_str()
        );
        let query = url.query_pairs().collect::<BTreeMap<_, _>>();
        assert_eq!(query["hide_saas_login"], "false");
        let device = query["device_id"].to_string();
        assert_eq!(device.len(), if site == Site::China { 16 } else { 19 });
        let mut callback = url::Url::parse(&query["auth_callback_url"]).unwrap();
        callback.query_pairs_mut().append_pair("userJwt",&json!({"Token":"browser-jwt","RefreshToken":"same-refresh","TokenExpireAt":"2099-01-01T00:00:00Z"}).to_string()).append_pair("userInfo",&json!({"UserID":"member","ScreenName":"member@example.com","Host":if site==Site::China{"https://tenant.trae.cn"}else{"https://coreva-normal.trae.ai"},"Region":"US","TenantID":"tenant","TenantToken":"must-not-be-kept"}).to_string());
        let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", callback.port().unwrap()))
            .await
            .unwrap();
        let request = format!(
            "GET {}?{} HTTP/1.1\r\nHost: localhost\r\n\r\n",
            callback.path(),
            callback.query().unwrap()
        );
        socket.write_all(request.as_bytes()).await.unwrap();
        let mut reply = Vec::new();
        socket.read_to_end(&mut reply).await.unwrap();
        assert!(String::from_utf8(reply).unwrap().contains("completed"));
        let result = callback_result(&provider, &flow).await;
        let credentials = &result["credentials"];
        assert_eq!(credentials["deviceId"], device);
        assert_eq!(credentials["uid"], "member");
        assert_eq!(credentials["account"]["TenantID"], "tenant");
        assert!(credentials["account"].get("TenantToken").is_none());
        assert!(credentials.get("desktopDevice").is_none());
        if site == Site::China {
            assert_eq!(credentials["access"], "device-bound");
            let requests = client.requests.lock().unwrap();
            assert!(requests[0]
                .url
                .ends_with("/trae/api/v3/oauth/ExchangeToken"));
            let body: Value = serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
            assert_eq!(body["DeviceInfo"]["DeviceID"], device);
            assert_eq!(body["ClientID"], "en1oxy7wnw8j9n");
        } else {
            assert_eq!(site.api(credentials), "https://coreva-normal.trae.ai");
            assert!(client.requests.lock().unwrap().is_empty());
        }
    }
}
async fn callback_result(provider: &Trae, flow: &Value) -> Value {
    callback(provider, json!({"session":flow["session"]}))
        .await
        .unwrap()
}
#[tokio::test]
async fn old_refresh_only_enterprise_callbacks_preserve_callback_client_host_and_region() {
    let client = Client::new(vec![(200, token("exchanged", "next-refresh"))]);
    let query=BTreeMap::from([("refreshToken".into(),"old-refresh".into()),("clientId".into(),"enterprise-client".into()),("authHost".into(),"https://login.trae.ai/proxy".into()),("userInfo".into(),json!({"UserID":"member","ScreenName":"User","Region":"US","Host":"https://coreva-normal.trae.ai"}).to_string())]);
    let result = signed_in(
        client.as_ref(),
        Site::Global,
        &query,
        &device(Site::Global).unwrap(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(result["region"], "US");
    assert_eq!(result["uid"], "member");
    assert_eq!(result["clientId"], "enterprise-client");
    assert_eq!(result["authHost"], "https://login.trae.ai/proxy");
    let requests = client.requests.lock().unwrap();
    assert!(requests[0]
        .url
        .starts_with("https://login.trae.ai/proxy/cloudide/"));
}
#[tokio::test]
async fn cn_renewal_tries_bound_clients_before_legacy_and_keeps_generated_device_keys() {
    let client = Client::new(vec![
        (400, json!({"code":1})),
        (400, json!({"code":1})),
        (200, token("legacy-access", "next-refresh")),
    ]);
    let provider = configured(Site::China, client.clone());
    let previous = credentials();
    let next = renew(&provider, client.as_ref(), &previous).await.unwrap();
    assert_eq!(next["access"], "legacy-access");
    assert_eq!(next["deviceId"], previous["deviceId"]);
    assert!(!text(&next, "devicePrivateKey").is_empty());
    assert_eq!(client.requests.lock().unwrap().len(), 3);
    let again = renew(&provider, client.as_ref(), &previous).await.unwrap();
    assert_eq!(next, again);
    assert_eq!(client.requests.lock().unwrap().len(), 3);
}
#[tokio::test]
async fn refresh_returns_only_after_the_host_saves_rotated_credentials() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let client = Client::new(vec![(200, token("new-access", "new-refresh"))]);
    let provider = Arc::new(configured(Site::Global, client.clone()));
    let (host, plugin) = tokio::io::duplex(64 * 1024);
    let (input, output) = tokio::io::split(plugin);
    let task = tokio::spawn(nora_provider_plugin_sdk::serve_io(provider, input, output));
    let (input, mut output) = tokio::io::split(host);
    let mut input = BufReader::new(input);
    let mut line = String::new();
    let manifest: Value =
        serde_json::from_str(include_str!("../../trae-global/nora-provider.json")).unwrap();
    output.write_all(format!("{}\n",json!({"id":1,"method":"initialize","params":{"apiVersion":1,"pluginID":"trae-global","pluginDirectory":"/test/plugin","dataDirectory":"/test/data","manifest":manifest,"hostCapabilities":{"credentialUpdates":true}}})).as_bytes()).await.unwrap();
    input.read_line(&mut line).await.unwrap();
    line.clear();
    output
        .write_all(
            format!(
                "{}\n",
                json!({"id":2,"method":"refresh","params":{"credentials":credentials()}})
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
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["result"]["credentials"]["access"],
        "new-access"
    );
    drop(output);
    drop(input);
    task.abort();
}
