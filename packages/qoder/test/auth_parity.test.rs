use super::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn pat_credentials() -> Value {
    let mut value = credentials();
    value["patAuth"] = json!(true);
    value["pat"] = json!("pt-fixture");
    value["deviceToken"] = json!("");
    value["deviceRefresh"] = json!("");
    value
}

async fn rpc(provider: Qoder, site: Site, method: &str, params: Value) -> (Value, Vec<Value>) {
    let (host, plugin) = tokio::io::duplex(64 * 1024);
    let (input, output) = tokio::io::split(plugin);
    let task = tokio::spawn(nora_provider_plugin_sdk::serve_io(
        Arc::new(provider),
        input,
        output,
    ));
    let (input, mut output) = tokio::io::split(host);
    let mut input = BufReader::new(input);
    let mut line = String::new();
    let manifest: Value = serde_json::from_str(if site == Site::China {
        include_str!("../../qoder-cn/nora-provider.json")
    } else {
        include_str!("../nora-provider.json")
    })
    .unwrap();
    output.write_all(format!("{}\n",json!({"id":1,"method":"initialize","params":{"apiVersion":1,"pluginID":site.id(),"pluginDirectory":"/test/plugin","dataDirectory":"/test/data","manifest":manifest,"hostCapabilities":{"credentialUpdates":true}}})).as_bytes()).await.unwrap();
    input.read_line(&mut line).await.unwrap();
    assert!(serde_json::from_str::<Value>(&line)
        .unwrap()
        .get("result")
        .is_some());
    output
        .write_all(format!("{}\n", json!({"id":2,"method":method,"params":params})).as_bytes())
        .await
        .unwrap();
    let mut saved = Vec::new();
    let result = loop {
        line.clear();
        tokio::time::timeout(Duration::from_secs(2), input.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert!(!line.is_empty(), "provider ended before answering {method}");
        let value: Value = serde_json::from_str(&line).unwrap();
        if value["event"] == "credentials" {
            saved.push(value["credentials"].clone());
            output.write_all(format!("{}\n",json!({"id":0,"method":"credentialsSaved","params":{"id":2,"updateID":value["updateID"],"success":true}})).as_bytes()).await.unwrap();
        } else if value["id"] == 2 {
            break value;
        }
    };
    task.abort();
    (result, saved)
}

#[tokio::test]
async fn both_sites_accept_pat_token_aliases_dates_and_browser_account_identity() {
    for site in [Site::International, Site::China] {
        for access_key in ["token", "device_token", "access_token"] {
            for expiry_key in ["expires_at", "expire_time"] {
                let mut token = json!({"refreshToken":"rotated-refresh"});
                token[access_key] = json!("rotated-access");
                token[expiry_key] = json!("2099-01-01T00:00:00Z");
                let email = if site == Site::China {
                    ""
                } else {
                    "member@example.com"
                };
                let client = Client::new(vec![
                    (200, token),
                    (200, json!({"id":"same-user","email":email,"name":"Member"})),
                ]);
                let provider = configured(site, client.clone());
                let flow = authorize(
                    &provider,
                    json!({"authMethodID":"pat","inputs":{"pat":"pt-fixture"}}),
                )
                .await
                .unwrap();
                let result = callback(&provider, json!({"session":flow["session"]}))
                    .await
                    .unwrap();
                let value = &result["credentials"];
                assert_eq!(value["access"], "rotated-access");
                assert_eq!(value["refresh"], "rotated-refresh");
                assert_eq!(
                    value["expires"],
                    usage::date(&json!("2099-01-01T00:00:00Z")).unwrap()
                );
                assert_eq!(
                    value["accountId"],
                    if email.is_empty() { "same-user" } else { email }
                );
                assert_eq!(value["uid"], "same-user");
                assert_eq!(value["patAuth"], true);
                assert_eq!(value["deviceToken"], "");
                assert!(value.get("deviceChat").is_none());
                let requests = client.requests.lock().unwrap();
                assert!(requests.iter().all(|request| request
                    .url
                    .starts_with(&site.deployment(None).unwrap().openapi)));
                assert_eq!(
                    serde_json::from_slice::<Value>(requests[0].body.as_ref().unwrap()).unwrap(),
                    json!({"personal_token":"pt-fixture"})
                );
                assert_eq!(
                    requests[1].headers["authorization"],
                    "Bearer rotated-access"
                );
            }
        }
    }
}

#[tokio::test]
async fn both_regions_distinguish_refused_pat_and_refresh_tokens_from_service_failures() {
    for site in [Site::International, Site::China] {
        for status in [401, 403, 500] {
            for pat in [false, true] {
                let mut replies = vec![(status, json!({}))];
                if pat && status != 500 {
                    replies.extend([
                        (
                            200,
                            json!({"token":"new-access","refresh_token":"new-refresh"}),
                        ),
                        (200, json!({"id":"u1","email":"one@example.com"})),
                    ]);
                }
                let client = Client::new(replies);
                let provider = configured(site, client.clone());
                let previous = if pat {
                    pat_credentials()
                } else {
                    credentials()
                };
                let result = renew(&provider, client.as_ref(), &previous, false).await;
                if pat && status != 500 {
                    let next = result.unwrap();
                    assert_eq!(next["access"], "new-access");
                    for field in ["machineId", "accountId", "uid", "pat", "patAuth"] {
                        assert_eq!(next[field], previous[field], "changed {field}");
                    }
                    assert_eq!(client.requests.lock().unwrap().len(), 3);
                } else {
                    assert_eq!(
                        result.unwrap_err().code,
                        if status == 500 {
                            "upstream_transport_error"
                        } else {
                            "authentication_required"
                        }
                    );
                    assert_eq!(client.requests.lock().unwrap().len(), 1);
                    assert!(provider.spent.lock().await.is_empty());
                }
                assert!(client.requests.lock().unwrap().iter().all(|request| request
                    .url
                    .starts_with(&site.deployment(None).unwrap().openapi)));
            }
        }
        for status in [400, 401, 403, 503] {
            let client = Client::new(vec![(401, json!({})), (status, json!({}))]);
            let provider = configured(site, client.clone());
            let error = renew(&provider, client.as_ref(), &pat_credentials(), false)
                .await
                .unwrap_err();
            assert_eq!(
                error.code,
                if status == 503 {
                    "upstream_transport_error"
                } else {
                    "authentication_required"
                }
            );
            assert_eq!(client.requests.lock().unwrap().len(), 2);
            assert!(provider.spent.lock().await.is_empty());
        }
    }
}

#[tokio::test]
async fn failed_pat_identity_never_creates_an_account_or_caches_a_rotation() {
    for site in [Site::International, Site::China] {
        for profile in [json!({"email":"no-id@example.com"}), json!({"id":""})] {
            let client = Client::new(vec![
                (
                    200,
                    json!({"access_token":"new","refreshToken":"new-refresh"}),
                ),
                (200, profile),
            ]);
            let provider = configured(site, client.clone());
            let flow = authorize(
                &provider,
                json!({"authMethodID":"pat","inputs":{"pat":"pt-fixture"}}),
            )
            .await
            .unwrap();
            assert!(callback(&provider, json!({"session":flow["session"]}))
                .await
                .is_err());
            assert!(provider.spent.lock().await.is_empty());
        }
    }
}

#[tokio::test]
async fn device_refresh_alias_rotates_both_pairs_with_seconds_expiry_on_both_sites() {
    for site in [Site::International, Site::China] {
        let client = Client::new(vec![(
            200,
            json!({"device_token":"device-new","refresh_token":"device-refresh-new","expires_in":7200}),
        )]);
        let provider = configured(site, client.clone());
        let mut previous = credentials();
        previous["deviceChat"] = json!(true);
        let before = now();
        let next = renew(&provider, client.as_ref(), &previous, false)
            .await
            .unwrap();
        assert_eq!(next["access"], next["deviceToken"]);
        assert_eq!(next["refresh"], next["deviceRefresh"]);
        assert_eq!(next["access"], "device-new");
        assert!(next["expires"].as_u64().unwrap() >= before + 7_200_000);
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].url.ends_with("/api/v1/deviceToken/refresh"));
    }
}

#[tokio::test]
async fn expired_pat_usage_renews_and_saves_before_reading_the_account_page() {
    for site in [Site::International, Site::China] {
        let client = Client::new(vec![
            (401, json!({})),
            (
                200,
                json!({"access_token":"new-access","refreshToken":"new-refresh","expire_time":"2099-01-01T00:00:00Z"}),
            ),
            (200, json!({"id":"u1","email":"one@example.com"})),
            (
                200,
                json!({"displayMode":"qoder","qoderUsage":{"userType":"pro","userQuota":{"name":"Credits","total":100,"used":10}}}),
            ),
        ]);
        let provider = configured(site, client.clone());
        let mut previous = pat_credentials();
        previous["expires"] = json!(now() - 1000);
        let (result, saved) = rpc(
            provider,
            site,
            "usage",
            json!({"account":"one","credentials":previous.clone()}),
        )
        .await;
        assert!(result.get("error").is_none(), "{result}");
        assert_eq!(result["result"]["windows"][0]["used"], 10.0);
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0]["access"], "new-access");
        assert_eq!(saved[0]["refresh"], "new-refresh");
        for field in ["machineId", "accountId", "pat", "patAuth"] {
            assert_eq!(saved[0][field], previous[field]);
        }
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert!(requests[0].url.ends_with("/api/v1/jobToken/refresh"));
        assert!(requests[3].url.ends_with("/sash/api/v2/me/usage"));
        assert_eq!(requests[3].headers["authorization"], "Bearer new-access");
    }
}

#[tokio::test]
async fn a_stale_credential_follows_all_successful_rotations_without_replaying_refresh_tokens() {
    let client = Client::new(vec![
        (
            200,
            json!({"token":"access-two","refresh_token":"refresh-two","expires_in":3600000}),
        ),
        (
            200,
            json!({"token":"access-three","refresh_token":"refresh-three","expires_in":3600000}),
        ),
    ]);
    let provider = configured(Site::International, client.clone());
    let initial = credentials();
    let second = renew(&provider, client.as_ref(), &initial, false)
        .await
        .unwrap();
    let third = renew(&provider, client.as_ref(), &second, false)
        .await
        .unwrap();
    let repeated = renew(&provider, client.as_ref(), &initial, false)
        .await
        .unwrap();
    assert_eq!(repeated, third);
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn model_discovery_renews_nearly_expired_credentials_before_its_signed_request() {
    for site in [Site::International, Site::China] {
        let client = Client::new(vec![
            (
                200,
                json!({"token":"new-access","refresh_token":"new-refresh","expires_in":3_600_000}),
            ),
            (200, crate::tests::catalog()),
        ]);
        let provider = configured(site, client.clone());
        let mut previous = credentials();
        previous["expires"] = json!(now() + 240_000);
        let (result, saved) = rpc(
            provider,
            site,
            "models",
            json!({"account":"one","credentials":previous}),
        )
        .await;
        assert!(result.get("error").is_none(), "{result}");
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0]["access"], "new-access");
        assert_eq!(result["result"]["models"][0]["id"], "model");
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].url.ends_with("/api/v1/jobToken/refresh"));
        assert!(requests[1].url.ends_with(MODEL_PATH));
    }
}

#[tokio::test]
async fn an_expired_cached_rotation_renews_the_latest_pair_instead_of_the_original() {
    let client = Client::new(vec![
        (
            200,
            json!({"token":"short-access","refresh_token":"short-refresh","expires_in":1}),
        ),
        (
            200,
            json!({"token":"live-access","refresh_token":"live-refresh","expires_in":3_600_000}),
        ),
    ]);
    let provider = configured(Site::International, client.clone());
    let original = credentials();
    renew(&provider, client.as_ref(), &original, false)
        .await
        .unwrap();
    let next = renew(&provider, client.as_ref(), &original, false)
        .await
        .unwrap();
    assert_eq!(next["access"], "live-access");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let tokens = requests
        .iter()
        .map(|request| {
            serde_json::from_slice::<Value>(request.body.as_ref().unwrap()).unwrap()
                ["refresh_token"]
                .clone()
        })
        .collect::<Vec<_>>();
    assert_eq!(tokens, vec![json!("refresh-one"), json!("short-refresh")]);
}
