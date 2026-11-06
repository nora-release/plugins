use super::*;
use nora_ai::{HttpClientError, HttpRequest};
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
struct Client {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<(u16, Value)>>,
    pause: Mutex<Option<Arc<RefreshPause>>>,
}
#[derive(Default)]
struct RefreshPause {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        let (status, value) = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected upstream request");
        let data = serde_json::to_vec(&value).unwrap();
        let pause = self.pause.lock().unwrap().take();
        if let Some(pause) = pause {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
        Ok(HttpStreamingResponse {
            status,
            headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
            body: Box::pin(futures::stream::once(async move { Ok(data.into()) })),
        })
    }
}
struct Store {
    saved: Mutex<Vec<Value>>,
    failures: AtomicUsize,
}
#[async_trait]
impl core::CredentialStore for Store {
    async fn save(&self, value: Value) -> PluginResult<()> {
        self.saved.lock().unwrap().push(value);
        if self
            .failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(PluginError::transport("Durable save was refused"));
        }
        Ok(())
    }
}
fn setup(values: Vec<(u16, Value)>) -> (MiniMaxCode, Arc<Client>, Arc<Store>) {
    let client = Arc::new(Client {
        requests: Mutex::new(Vec::new()),
        replies: Mutex::new(values.into()),
        pause: Mutex::new(None),
    });
    let store = Arc::new(Store {
        saved: Mutex::new(Vec::new()),
        failures: AtomicUsize::new(0),
    });
    let core = Core::with_dependencies("minimax-code", client.clone(), store.clone());
    core.initialize(InitializeParams {
        subscription_accounts_enabled: true,
        installed_manifest: None,
        runtime_config: serde_json::Value::Null,
        api_version: 2,
        plugin_id: "minimax-code".into(),
        plugin_directory: PathBuf::new(),
        data_directory: PathBuf::new(),
        manifest: serde_json::from_str(include_str!("../nora-plugin.json")).unwrap(),
    })
    .unwrap();
    (MiniMaxCode { core }, client, store)
}
fn credentials() -> Value {
    json!({"site":"minimax-code","type":"oauth","access":"old-access","refresh":"old-refresh","expires":core::now()+3600_000,"realUserID":"user-one","email":"a@example.com"})
}
#[tokio::test]
async fn refresh_rotation_is_durable_before_the_new_token_can_be_used() {
    let (plugin, client, store) = setup(vec![(
        200,
        json!({"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}),
    )]);
    let old = credentials();
    let next = plugin.live(&old, client.as_ref(), true).await.unwrap();
    assert_eq!(next["access"], "new-access");
    assert_eq!(store.saved.lock().unwrap().len(), 1);
    assert_eq!(
        plugin.live(&old, client.as_ref(), true).await.unwrap()["access"],
        "new-access"
    );
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    let requests = client.requests.lock().unwrap();
    let body = String::from_utf8(requests[0].body.clone().unwrap()).unwrap();
    assert!(body.contains("refresh_token=old-refresh"));
    assert!(requests[0].url.ends_with("/oauth2/token"));
}
#[tokio::test]
async fn failed_host_ack_retries_storage_without_spending_a_refresh_token_twice() {
    let (plugin, client, store) = setup(vec![(
        200,
        json!({"access_token":"new","refresh_token":"rotated","expires_in":3600}),
    )]);
    store.failures.store(1, Ordering::SeqCst);
    let old = credentials();
    assert!(plugin.live(&old, client.as_ref(), true).await.is_err());
    assert_eq!(
        plugin.live(&old, client.as_ref(), true).await.unwrap()["access"],
        "new"
    );
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    assert_eq!(store.saved.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn only_invalid_grant_at_http_400_expires_the_account() {
    let (plugin, client, _) = setup(vec![(400, json!({"error":"invalid_grant"}))]);
    assert_eq!(
        plugin
            .live(&credentials(), client.as_ref(), true)
            .await
            .unwrap_err()
            .code,
        "authentication_required"
    );
    let (plugin, client, _) = setup(vec![(503, json!({"error":"temporarily_unavailable"}))]);
    assert_eq!(
        plugin
            .live(&credentials(), client.as_ref(), true)
            .await
            .unwrap_err()
            .code,
        "upstream_transport_error"
    );
}
#[tokio::test]
async fn a_cn_account_is_not_silently_reused_for_the_global_site() {
    let (plugin, client, _) = setup(vec![]);
    let mut value = credentials();
    value["site"] = json!("minimax-code-global");
    assert!(plugin.live(&value, client.as_ref(), false).await.is_err());
    assert!(client.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn refused_account_model_listing_retries_the_real_public_model_configuration() {
    let (plugin, client, _) = setup(vec![
        (401, json!({})),
        (
            200,
            json!({"providers":[{"providerId":"minimax","config":{"models":{"new-model":{"name":"Actual model","reasoning":false}}}}]}),
        ),
    ]);
    let result = plugin
        .models(CallContext {
            subscription_accounts_enabled: true,
            account: "one".into(),
            credentials: Some(credentials()),
            network_proxy: None,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(result["models"][0]["id"], "new-model");
    assert_eq!(result["source"], "remote");
    let requests = client.requests.lock().unwrap();
    assert!(requests[0].headers.contains_key("authorization"));
    assert!(!requests[1].headers.contains_key("authorization"));
}
#[tokio::test]
async fn quota_refusals_are_retryable_and_native_tools_are_unchanged() {
    let (plugin, client, _) = setup(vec![(
        403,
        json!({"error":{"message":"insufficient credits"}}),
    )]);
    let body = json!({"model":"MiniMax-M3","messages":[{"role":"user","content":"Hello"}],"tools":[{"name":"read","input_schema":{"type":"object"}}],"stop_sequences":["END"]});
    let result = plugin
        .fetch(FetchRequest {
            caller_user_agent: None,
            constraints: Default::default(),
            subscription_accounts_enabled: true,
            account: "one".into(),
            credentials: Some(credentials()),
            network_proxy: None,
            url: format!("{}/messages", Site::for_id("minimax-code").unwrap().base()),
            method: "POST".into(),
            headers: BTreeMap::new(),
            metadata: BTreeMap::from([("sessionID".into(), "my-session".into())]),
            body: Some(
                base64::engine::general_purpose::STANDARD
                    .encode(serde_json::to_vec(&body).unwrap()),
            ),
            body_file: None,
        })
        .await
        .unwrap();
    assert_eq!(result.status, 429);
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(requests[0].body.as_ref().unwrap()).unwrap(),
        body
    );
    assert_eq!(requests[0].headers["x-mavis-session-id"], "my-session");
    assert_eq!(requests[0].headers["x-api-key"], "sk-xxx");
    assert_eq!(requests[0].headers["authorization"], "Bearer old-access");
}

#[tokio::test]
async fn device_login_sends_real_pkce_fields_and_keeps_the_verifier_private() {
    let (plugin, client, _) = setup(vec![(
        200,
        json!({"user_code":"CODE-123","device_code":"private-device","verification_uri_complete":"https://account.minimax.cn/activate?code=CODE-123","expires_in":600,"interval":1}),
    )]);
    let result = plugin
        .authorize(json!({"authMethodID":"oauth","inputs":{}}))
        .await
        .unwrap();
    assert_eq!(result["method"], "auto");
    assert!(result["session"].as_str().is_some());
    assert!(result.get("credentials").is_none());
    let requests = client.requests.lock().unwrap();
    let form = String::from_utf8(requests[0].body.clone().unwrap()).unwrap();
    assert!(form.contains("client_id=mcode-public"));
    assert!(form.contains("code_challenge_method=S256"));
    assert!(!form.contains("code_verifier="));
}

#[tokio::test]
async fn device_approval_returns_credentials_only_after_the_real_account_is_identified() {
    let (plugin, client, _) = setup(vec![
        (
            200,
            json!({"access_token":"approved-token","refresh_token":"refresh","expires_in":3600}),
        ),
        (
            200,
            json!({"data":{"userInfo":{"realUserID":"real-user","userEmail":"person@example.com"}}}),
        ),
    ]);
    let session = plugin
        .core
        .begin(
            json!({"verifier":"private-verifier","poll":{"user_code":"CODE"},"interval":1}),
            core::now() + 10_000,
        )
        .await
        .unwrap();
    let result = plugin.callback(json!({"session":session})).await.unwrap();
    assert_eq!(result["credentials"]["accountId"], "real-user");
    assert_eq!(result["credentials"]["email"], "person@example.com");
    assert_eq!(result["credentials"]["site"], "minimax-code");
    let requests = client.requests.lock().unwrap();
    assert!(String::from_utf8(requests[0].body.clone().unwrap())
        .unwrap()
        .contains("code_verifier=private-verifier"));
    assert!(requests[1].headers.contains_key("yy"));
    assert_eq!(
        requests[1].headers["authorization"],
        "Bearer approved-token"
    );
}

#[tokio::test]
async fn successful_http_without_a_real_token_is_never_a_successful_login() {
    let (plugin, _, _) = setup(vec![(200, json!({"status":"ok"}))]);
    let session = plugin
        .core
        .begin(
            json!({"verifier":"v","poll":{"user_code":"CODE"},"interval":1}),
            core::now() + 10_000,
        )
        .await
        .unwrap();
    assert!(plugin.callback(json!({"session":session})).await.is_err());
}

#[tokio::test]
async fn a_request_and_explicit_renewal_share_one_in_flight_refresh_and_late_reads_use_its_pair() {
    let (plugin, client, store) = setup(vec![(
        200,
        json!({"access_token":"next","refresh_token":"rotated","expires_in":3600}),
    )]);
    let plugin = Arc::new(plugin);
    let pause = Arc::new(RefreshPause::default());
    *client.pause.lock().unwrap() = Some(pause.clone());
    let mut old = credentials();
    old["expires"] = json!(1);
    let first = {
        let (plugin, client, old) = (plugin.clone(), client.clone(), old.clone());
        tokio::spawn(async move { plugin.live(&old, client.as_ref(), false).await })
    };
    pause.entered.notified().await;
    let second = {
        let (plugin, client, old) = (plugin.clone(), client.clone(), old.clone());
        tokio::spawn(async move { plugin.live(&old, client.as_ref(), true).await })
    };
    tokio::task::yield_now().await;
    pause.release.notify_one();
    for result in [first.await.unwrap(), second.await.unwrap()] {
        assert_eq!(result.unwrap()["access"], "next");
    }
    assert_eq!(
        plugin.live(&old, client.as_ref(), false).await.unwrap()["refresh"],
        "rotated"
    );
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    assert_eq!(store.saved.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn minimax_accounts_keep_independent_rotation_chains() {
    let (plugin, client, store) = setup(vec![
        (
            200,
            json!({"access_token":"next-a","refresh_token":"rotated-a","expires_in":3600}),
        ),
        (
            200,
            json!({"access_token":"next-b","refresh_token":"rotated-b","expires_in":3600}),
        ),
    ]);
    let a = credentials();
    let mut b = credentials();
    b["access"] = json!("access-b");
    b["refresh"] = json!("refresh-b");
    b["realUserID"] = json!("user-two");
    let (a_new, b_new) = tokio::join!(
        plugin.live(&a, client.as_ref(), true),
        plugin.live(&b, client.as_ref(), true)
    );
    assert_eq!(a_new.unwrap()["access"], "next-a");
    assert_eq!(b_new.unwrap()["access"], "next-b");
    assert_eq!(
        plugin.live(&a, client.as_ref(), true).await.unwrap()["refresh"],
        "rotated-a"
    );
    assert_eq!(
        plugin.live(&b, client.as_ref(), true).await.unwrap()["refresh"],
        "rotated-b"
    );
    assert_eq!(client.requests.lock().unwrap().len(), 2);
    assert_eq!(store.saved.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn temporary_minimax_refresh_errors_preserve_a_live_token_but_do_not_use_an_expired_one() {
    for (status, body) in [
        (500, json!({"error":"server_error"})),
        (401, json!({"error":"invalid_grant"})),
        (400, json!({"error":"invalid_request"})),
        (503, json!({})),
    ] {
        for expired in [false, true] {
            let (plugin, client, store) = setup(vec![(status, body.clone())]);
            let mut old = credentials();
            old["expires"] = json!(if expired { 1 } else { core::now() + 30_000 });
            let result = plugin.live(&old, client.as_ref(), false).await;
            if expired {
                assert_eq!(result.unwrap_err().code, "upstream_transport_error");
            } else {
                assert_eq!(result.unwrap(), old);
            }
            assert!(store.saved.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn repeated_minimax_storage_failures_keep_the_same_pair_until_the_host_accepts_it() {
    let (plugin, client, store) = setup(vec![(
        200,
        json!({"access_token":"next","refresh_token":"rotated","expires_in":3600}),
    )]);
    store.failures.store(2, Ordering::SeqCst);
    let old = credentials();
    for _ in 0..2 {
        assert!(plugin.live(&old, client.as_ref(), true).await.is_err());
    }
    assert_eq!(
        plugin.live(&old, client.as_ref(), true).await.unwrap()["access"],
        "next"
    );
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    let saved = store.saved.lock().unwrap();
    assert_eq!(saved.len(), 3);
    assert!(saved
        .iter()
        .all(|value| value["access"] == "next" && value["refresh"] == "rotated"));
}

fn checkin_week(today: u64, today_status: u64) -> Value {
    json!({"base_resp":{"status_code":0},"data":{"days":(1..=7).map(|day| json!({"day_no":day,"points":10,"status":if day < today {3} else if day == today {today_status} else {1},"is_today":day == today})).collect::<Vec<_>>()}})
}

#[tokio::test]
async fn status_only_checks_the_vendor_day_without_claiming_or_guessing_a_reward() {
    for (status, outcome) in [(2, "ready"), (3, "done"), (4, "ineligible")] {
        let (plugin, client, _) = setup(vec![(200, checkin_week(3, status))]);
        let result = plugin
            .call("checkinStatus", json!({"credentials":credentials()}))
            .await
            .unwrap();
        assert_eq!(result["outcome"], outcome);
        if outcome == "ready" {
            assert_eq!(result["retrySafe"], true);
            assert!(result.get("credit").is_none());
        }
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].url.contains("timezone_id=Asia%2FShanghai"));
        assert_eq!(requests[0].method, HttpMethod::Get);
    }
}

#[tokio::test]
async fn daily_checkin_claims_only_available_days_and_reports_actual_reward_and_streak() {
    for day in [1, 3, 7] {
        for result in [1, 2] {
            let (plugin, client, _) = setup(vec![
                (200, checkin_week(day, 2)),
                (
                    200,
                    json!({"base_resp":{"status_code":0},"data":{"claim_result":result,"points":20,"day_no":day}}),
                ),
            ]);
            let value = plugin
                .call("checkin", json!({"credentials":credentials()}))
                .await
                .unwrap();
            assert_eq!(
                value,
                json!({"outcome":if result == 1 {"claimed"} else {"done"},"credit":20,"streak":day})
            );
            let requests = client.requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert!(requests[0]
                .url
                .starts_with(Site::for_id("minimax-code").unwrap().llm));
            assert!(requests[0]
                .url
                .contains("/minimax-cloud/api/v1/signin/status"));
            assert!(requests[1]
                .url
                .contains("/minimax-cloud/api/v1/signin/claim"));
            assert_eq!(requests[1].body.as_deref(), Some(b"{}".as_slice()));
            for request in requests.iter() {
                assert_eq!(request.headers["authorization"], "Bearer old-access");
                for header in ["yy", "x-timestamp", "x-signature"] {
                    assert!(!request.headers[header].is_empty());
                }
                let url = url::Url::parse(&request.url).unwrap();
                assert!(url
                    .query_pairs()
                    .any(|(key, value)| key == "device_platform" && value == "mcode"));
            }
        }
    }
}

#[tokio::test]
async fn finished_or_ineligible_checkin_panels_never_send_a_claim() {
    for (today, status, expected) in [
        (1, 3, json!({"outcome":"done","credit":10,"streak":1})),
        (4, 3, json!({"outcome":"done","credit":10,"streak":4})),
        (2, 4, json!({"outcome":"ineligible"})),
        (2, 1, json!({"outcome":"inactive"})),
        (8, 1, json!({"outcome":"inactive"})),
    ] {
        let (plugin, client, _) = setup(vec![(200, checkin_week(today, status))]);
        assert_eq!(
            plugin
                .call("checkin", json!({"credentials":credentials()}))
                .await
                .unwrap(),
            expected
        );
        assert_eq!(client.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn malformed_or_failed_checkin_panels_never_claim_or_report_a_reward() {
    for (status, body) in [
        (200, json!({})),
        (200, json!({"data":{"days":[]}})),
        (
            200,
            json!({"base_resp":{"status_code":1004,"status_msg":"login fail"}}),
        ),
        (400, json!({"error":"invalid signature"})),
        (401, json!({"error":"denied"})),
        (403, json!({"error":"forbidden"})),
        (502, json!({"message":"internal"})),
    ] {
        let (plugin, client, store) = setup(vec![(status, body)]);
        assert!(plugin
            .call("checkin", json!({"credentials":credentials()}))
            .await
            .is_err());
        assert_eq!(client.requests.lock().unwrap().len(), 1);
        assert!(store.saved.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn unknown_checkin_claim_results_are_errors_and_missing_credentials_never_reach_network() {
    let (plugin, client, store) = setup(vec![
        (200, checkin_week(1, 2)),
        (
            200,
            json!({"base_resp":{"status_code":0},"data":{"claim_result":9,"points":999}}),
        ),
    ]);
    assert!(plugin
        .call("checkin", json!({"credentials":credentials()}))
        .await
        .is_err());
    assert_eq!(client.requests.lock().unwrap().len(), 2);
    assert!(store.saved.lock().unwrap().is_empty());
    let (plugin, client, _) = setup(vec![]);
    for params in [
        json!({}),
        json!({"credentials":{"type":"api","key":"fixture-key"}}),
    ] {
        assert!(plugin.call("checkin", params).await.is_err());
    }
    assert!(client.requests.lock().unwrap().is_empty());
}
