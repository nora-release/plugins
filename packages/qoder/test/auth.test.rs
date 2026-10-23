use super::*;
use crate::tests::{configured, credentials, Client};

#[path = "auth_parity.test.rs"]
mod parity;
#[tokio::test]
async fn device_sign_in_uses_each_regions_pkce_and_its_own_job_exchange() {
    for site in [Site::International, Site::China] {
        let client = Client::new(vec![
            (
                200,
                json!({"token":"device","refresh_token":"device-refresh","user_id":"u1"}),
            ),
            (
                200,
                json!({"token":"job","refresh_token":"job-refresh","expires_in":3600000}),
            ),
            (
                200,
                json!({"id":"u1","email":"member@example.com","name":"Member"}),
            ),
        ]);
        let provider = configured(site, client.clone());
        let flow = authorize(&provider, json!({"authMethodID":"oauth"}))
            .await
            .unwrap();
        let url = url::Url::parse(flow["url"].as_str().unwrap()).unwrap();
        let query = url.query_pairs().collect::<BTreeMap<_, _>>();
        assert_eq!(query["challenge_method"], "S256");
        assert_eq!(query["client_id"], site.client_id());
        assert!(!query.contains_key("redirect_uri"));
        let result = callback(&provider, json!({"session":flow["session"]}))
            .await
            .unwrap();
        assert_eq!(result["credentials"]["access"], "job");
        assert_eq!(result["credentials"]["accountId"], "member@example.com");
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(requests.iter().all(|request| request
            .url
            .starts_with(&site.deployment(None).unwrap().openapi)));
        assert!(requests[0].url.contains("verifier="));
        assert_eq!(requests[1].headers["authorization"], "Bearer device");
    }
}
#[tokio::test]
async fn cn_enterprise_flow_keeps_private_hosts_and_device_chat_when_job_exchange_is_refused() {
    let client = Client::new(vec![
        (
            200,
            json!({"token":"device","refresh_token":"refresh","expires_in":3600}),
        ),
        (403, json!({})),
        (200, json!({"uid":"u1","email":"member@example.com"})),
    ]);
    let provider = configured(Site::China, client.clone());
    let flow = authorize(
        &provider,
        json!({"authMethodID":"vpc","inputs":{"vpc":"acme"}}),
    )
    .await
    .unwrap();
    assert!(flow["url"]
        .as_str()
        .unwrap()
        .starts_with("https://acme.vpc.qoder.com.cn/"));
    let result = callback(&provider, json!({"session":flow["session"]}))
        .await
        .unwrap();
    assert_eq!(result["credentials"]["deviceChat"], true);
    assert_eq!(result["credentials"]["access"], "device");
    assert_eq!(
        result["credentials"]["accountId"],
        "member@example.com (acme)"
    );
    assert!(client.requests.lock().unwrap().iter().all(|request| request
        .url
        .starts_with("https://acme-openapi.vpc.qoder.com.cn/")));
}
#[tokio::test]
async fn personal_token_exchange_supports_live_token_aliases_and_refresh_fallback() {
    let client = Client::new(vec![
        (
            200,
            json!({"access_token":"exchanged","refreshToken":"job-refresh","expires_in":3600000}),
        ),
        (200, json!({"id":"u1","email":"one@example.com"})),
        (401, json!({})),
        (
            200,
            json!({"token":"reexchanged","refresh_token":"new-refresh"}),
        ),
        (200, json!({"id":"u1","email":"one@example.com"})),
    ]);
    let provider = configured(Site::International, client.clone());
    let flow = authorize(
        &provider,
        json!({"authMethodID":"pat","inputs":{"pat":"pt-secret"}}),
    )
    .await
    .unwrap();
    let credentials = callback(&provider, json!({"session":flow["session"]}))
        .await
        .unwrap()["credentials"]
        .clone();
    assert_eq!(credentials["patAuth"], true);
    let renewed = renew(&provider, client.as_ref(), &credentials, false)
        .await
        .unwrap();
    assert_eq!(renewed["access"], "reexchanged");
    assert_eq!(renewed["machineId"], credentials["machineId"]);
    assert_eq!(renewed["accountId"], credentials["accountId"]);
    assert_eq!(client.requests.lock().unwrap().len(), 5);
}
#[tokio::test]
async fn concurrent_refreshes_spend_one_token_once_and_device_chat_rotates_both_pairs() {
    for device in [false, true] {
        let client = Client::new(vec![(
            200,
            json!({"token":"new-access","refresh_token":"new-refresh","expires_in":if device {3600} else {3_600_000}}),
        )]);
        let provider = configured(Site::China, client.clone());
        let mut credentials = credentials();
        credentials["deviceChat"] = json!(device);
        let (a, b) = tokio::join!(
            renew(&provider, client.as_ref(), &credentials, false),
            renew(&provider, client.as_ref(), &credentials, false)
        );
        assert_eq!(a.as_ref().unwrap(), b.as_ref().unwrap());
        assert_eq!(a.as_ref().unwrap()["access"], "new-access");
        if device {
            assert_eq!(a.unwrap()["deviceToken"], "new-access");
        }
        assert_eq!(client.requests.lock().unwrap().len(), 1);
    }
}
#[tokio::test]
async fn invalid_or_expired_flows_do_not_reach_network() {
    let client = Client::new(vec![]);
    let provider = configured(Site::China, client.clone());
    assert!(authorize(
        &provider,
        json!({"authMethodID":"vpc","inputs":{"vpc":"https://attacker.example"}})
    )
    .await
    .is_err());
    assert!(authorize(
        &provider,
        json!({"authMethodID":"pat","inputs":{"pat":"wrong"}})
    )
    .await
    .is_err());
    let flow = authorize(&provider, json!({"authMethodID":"oauth"}))
        .await
        .unwrap();
    provider
        .pending
        .lock()
        .await
        .get_mut(flow["session"].as_str().unwrap())
        .unwrap()
        .deadline = 0;
    assert!(callback(&provider, json!({"session":flow["session"]}))
        .await
        .is_err());
    assert!(client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn newly_rotated_tokens_are_not_acknowledged_before_the_host_saves_the_pair() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let client = Client::new(vec![(
        200,
        json!({"token":"new-access","refresh_token":"new-refresh","expires_in":3600000}),
    )]);
    let provider = Arc::new(configured(Site::International, client.clone()));
    let (host, plugin) = tokio::io::duplex(64 * 1024);
    let (input, output) = tokio::io::split(plugin);
    let task = tokio::spawn(nora_provider_plugin_sdk::serve_io(provider, input, output));
    let (input, mut output) = tokio::io::split(host);
    let mut input = BufReader::new(input);
    let mut line = String::new();
    let manifest: Value = serde_json::from_str(include_str!("../nora-provider.json")).unwrap();
    output.write_all(format!("{}\n",json!({"id":1,"method":"initialize","params":{"apiVersion":1,"pluginID":"qoder","pluginDirectory":"/test/plugin","dataDirectory":"/test/data","manifest":manifest,"hostCapabilities":{"credentialUpdates":true}}})).as_bytes()).await.unwrap();
    input.read_line(&mut line).await.unwrap();
    assert!(serde_json::from_str::<Value>(&line)
        .unwrap()
        .get("result")
        .is_some());
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
    let result: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(result["result"]["credentials"]["access"], "new-access");
    assert_eq!(
        result["result"]["credentials"]["machineId"],
        credentials()["machineId"]
    );
    drop(output);
    drop(input);
    task.abort();
}
