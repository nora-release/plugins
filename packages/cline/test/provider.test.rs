use super::*;
use base64::Engine;
use nora_ai::{HttpClientError, HttpStreamingResponse};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
struct Client {
    requests: Mutex<Vec<HttpRequest>>,
    times: Mutex<Vec<tokio::time::Instant>>,
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
        self.times.lock().unwrap().push(tokio::time::Instant::now());
        let (status, value) = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected Cline request");
        if value["_transport_error"] == true {
            return Err(HttpClientError::Transport("fixture unavailable".into()));
        }
        let bytes = match value["_raw"].as_str() {
            Some(raw) => raw.as_bytes().to_vec(),
            None => serde_json::to_vec(&value).unwrap(),
        };
        let pause = self.pause.lock().unwrap().take();
        if let Some(pause) = pause {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
        Ok(HttpStreamingResponse {
            status,
            headers: BTreeMap::from([(
                "content-type".into(),
                value["_content_type"]
                    .as_str()
                    .unwrap_or("application/json")
                    .into(),
            )]),
            body: Box::pin(futures::stream::once(async move { Ok(bytes.into()) })),
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
            return Err(PluginError::transport("Save failed"));
        }
        Ok(())
    }
}
fn setup(replies: Vec<(u16, Value)>) -> (Cline, Arc<Client>, Arc<Store>) {
    let client = Arc::new(Client {
        requests: Mutex::new(Vec::new()),
        times: Mutex::new(Vec::new()),
        replies: Mutex::new(replies.into()),
        pause: Mutex::new(None),
    });
    let store = Arc::new(Store {
        saved: Mutex::new(Vec::new()),
        failures: AtomicUsize::new(0),
    });
    let core = Core::with_dependencies("cline", client.clone(), store.clone());
    core.initialize(InitializeParams {
        subscription_accounts_enabled: true,
        installed_manifest: None,
        runtime_config: serde_json::Value::Null,
        api_version: 1,
        plugin_id: "cline".into(),
        plugin_directory: PathBuf::new(),
        data_directory: PathBuf::new(),
        manifest: serde_json::from_str(include_str!("../nora-provider.json")).unwrap(),
    })
    .unwrap();
    (Cline::with_core(core), client, store)
}
fn credentials() -> Value {
    json!({"provider":"cline","type":"oauth","access":"old-access","refresh":"old-refresh","expires":core::now()+3600_000,"uid":"uid-one","email":"me@example.com","accountId":"me@example.com"})
}

#[path = "pinning.test.rs"]
mod pinning_tests;

#[path = "pinning-protocol.test.rs"]
mod pinning_protocol_tests;

#[tokio::test]
async fn clinepass_rejects_oauth_before_any_network_request() {
    let (_, client, store) = setup(vec![]);
    let core = Core::with_dependencies("clinepass", client.clone(), store);
    core.initialize(InitializeParams {
        subscription_accounts_enabled: true,
        installed_manifest: None,
        runtime_config: serde_json::Value::Null,
        api_version: 1,
        plugin_id: "clinepass".into(),
        plugin_directory: PathBuf::new(),
        data_directory: PathBuf::new(),
        manifest: json!({"id":"clinepass","authMethods":[{"id":"apiKey","type":"api"}]}),
    })
    .unwrap();
    let error = Cline::with_core(core)
        .authorize(json!({"authMethodID":"oauth"}))
        .await
        .unwrap_err();
    assert_eq!(error.code, "method_not_supported");
    assert!(client.requests.lock().unwrap().is_empty());
}
#[test]
fn raw_api_keys_and_workos_tokens_are_distinguished() {
    assert_eq!(
        bearer(&json!({"type":"api","key":"raw-key"})).unwrap(),
        "raw-key"
    );
    assert_eq!(bearer(&credentials()).unwrap(), "workos:old-access");
    assert_eq!(
        bearer(&json!({"access":"WorkOS:already","type":"oauth"})).unwrap(),
        "WorkOS:already"
    );
}
#[test]
fn official_task_ids_are_stable_for_one_session_but_distinct_between_sessions() {
    let a = headers(Some("one"));
    assert_eq!(a["x-task-id"], headers(Some("one"))["x-task-id"]);
    assert_ne!(a["x-task-id"], headers(Some("two"))["x-task-id"]);
    assert_eq!(a["x-client-type"], "cline-cli");
    assert!(!headers(None).contains_key("x-task-id"));
}
#[test]
fn pinning_preserves_the_original_cache_prefix_and_caller_routing_options() {
    let body = b"{ \"model\" : \"cline-pass/deepseek-v4.1-flash\", \"messages\": [] }".to_vec();
    let chat: Value = serde_json::from_slice(&body).unwrap();
    let pinned = pinning::Pin::for_chat(&chat, Some(true), false)
        .unwrap()
        .apply(&body)
        .unwrap();
    assert!(pinned.starts_with(&body[..body.len() - 1]));
    assert_eq!(
        serde_json::from_slice::<Value>(&pinned).unwrap()["providerOptions"]["gateway"]["only"],
        json!(["deepseek"])
    );
    let owned = json!({"model":"cline-pass/glm-5.3-flash","provider":{"only":["mine"]}});
    assert!(pinning::Pin::for_chat(&owned, Some(true), false).is_none());
}
#[test]
fn entitlement_and_credit_errors_do_not_get_mislabeled_as_expired_tokens() {
    assert_eq!(
        failure(
            403,
            &json!({"error":{"code":"ENTITLEMENT_ERROR","message":"Not subscribed"}})
        )
        .0,
        403
    );
    assert_eq!(
        failure(402, &json!({"message":"insufficient balance"})).0,
        429
    );
    assert_eq!(
        failure(403, &json!({"message":"model token allowance exhausted"})).0,
        403
    );
    assert_eq!(failure(403, &json!({"message":"token expired"})).0, 401);
}
#[tokio::test]
async fn rotating_workos_refresh_is_saved_once_and_preserves_the_original_account() {
    let (plugin, client, store) = setup(vec![(
        200,
        json!({"success":true,"data":{"accessToken":"new","refreshToken":"rotated","expiresAt":core::iso(core::now()+3600_000),"userInfo":{"clineUserId":"uid-one","email":"new-email@example.com"}}}),
    )]);
    let old = credentials();
    let result = plugin.live(&old, client.as_ref(), true).await.unwrap();
    assert_eq!(result["access"], "new");
    assert_eq!(result["accountId"], "me@example.com");
    assert_eq!(
        plugin.live(&old, client.as_ref(), true).await.unwrap()["access"],
        "new"
    );
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    assert_eq!(store.saved.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn a_failed_ack_never_spends_the_rotated_refresh_again() {
    let (plugin, client, store) = setup(vec![(
        200,
        json!({"accessToken":"new","refreshToken":"rotated","expiresAt":core::iso(core::now()+3600_000),"userInfo":{"clineUserId":"uid-one"}}),
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
async fn credit_balance_is_kept_when_clinepass_is_absent() {
    let (plugin, client, _) = setup(vec![
        (
            200,
            json!({"success":true,"data":{"clineUserId":"uid-one","email":"me@example.com"}}),
        ),
        (200, json!({"success":true,"data":{"balance":12500000}})),
        (404, json!({"message":"no plan"})),
    ]);
    let result = plugin
        .usage(CallContext {
            subscription_accounts_enabled: true,
            account: "one".into(),
            credentials: Some(credentials()),
            network_proxy: None,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(result["balance"], "$12.50");
    assert!(result.get("quotaError").is_none());
    assert!(client
        .requests
        .lock()
        .unwrap()
        .iter()
        .all(|r| r.headers["authorization"] == "Bearer workos:old-access"));
}
#[tokio::test]
async fn native_tools_and_sampling_survive_nonstreaming_envelope_removal() {
    let (plugin, client, _) = setup(vec![(
        200,
        json!({"data":{"choices":[{"message":{"tool_calls":[{"id":"call","function":{"name":"read","arguments":"{}"}}]}}],"usage":{"prompt_tokens":4}}}),
    )]);
    let body = json!({"model":"maker/model","messages":[{"role":"tool","tool_call_id":"call","content":"done"}],"top_p":0.25,"stop":["END"],"tools":[{"type":"function","function":{"name":"read"}}]});
    let response = plugin
        .fetch(FetchRequest {
            caller_user_agent: None,
            constraints: Default::default(),
            subscription_accounts_enabled: true,
            account: "one".into(),
            credentials: Some(credentials()),
            network_proxy: None,
            url: format!("{BASE}/chat/completions"),
            method: "POST".into(),
            headers: BTreeMap::new(),
            metadata: BTreeMap::from([("sessionID".into(), "private-session".into())]),
            body: Some(
                base64::engine::general_purpose::STANDARD
                    .encode(serde_json::to_vec(&body).unwrap()),
            ),
            body_file: None,
        })
        .await
        .unwrap();
    let chunks = response.body.collect::<Vec<_>>().await;
    let data: Vec<u8> = chunks.into_iter().flat_map(Result::unwrap).collect();
    let value: Value = serde_json::from_slice(&data).unwrap();
    assert!(value["choices"][0]["message"]["tool_calls"].is_array());
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(requests[0].body.as_ref().unwrap()).unwrap(),
        body
    );
    assert!(!requests[0]
        .headers
        .values()
        .any(|value| value == "private-session"));
}

#[tokio::test]
async fn approved_workos_credentials_are_registered_with_cline_before_login_succeeds() {
    let (plugin, client, _) = setup(vec![
        (
            200,
            json!({"access_token":"workos-access","refresh_token":"workos-refresh"}),
        ),
        (
            200,
            json!({"success":true,"data":{"accessToken":"cline-access","refreshToken":"cline-refresh","expiresAt":core::iso(core::now()+3600_000),"userInfo":{"clineUserId":"real-user","email":"person@example.com"}}}),
        ),
    ]);
    let session = plugin
        .core
        .begin(
            json!({"device_code":"private-device","interval":0}),
            core::now() + 10_000,
        )
        .await
        .unwrap();
    let value = plugin.callback(json!({"session":session})).await.unwrap();
    assert_eq!(value["credentials"]["access"], "cline-access");
    assert_eq!(value["credentials"]["uid"], "real-user");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].url, TOKEN);
    assert!(String::from_utf8(requests[0].body.clone().unwrap())
        .unwrap()
        .contains("device_code=private-device"));
    assert_eq!(requests[1].url, format!("{BASE}/auth/register"));
    assert_eq!(
        serde_json::from_slice::<Value>(requests[1].body.as_ref().unwrap()).unwrap(),
        json!({"accessToken":"workos-access","refreshToken":"workos-refresh"})
    );
}

#[tokio::test]
async fn a_vendor_failure_in_a_success_status_envelope_is_never_saved_as_login_success() {
    let (plugin, _, store) = setup(vec![
        (200, json!({"access_token":"workos-access"})),
        (200, json!({"success":false,"error":"registration refused"})),
    ]);
    let session = plugin
        .core
        .begin(
            json!({"device_code":"device","interval":0}),
            core::now() + 10_000,
        )
        .await
        .unwrap();
    assert!(plugin.callback(json!({"session":session})).await.is_err());
    assert!(store.saved.lock().unwrap().is_empty());
}

#[tokio::test]
async fn explicit_and_request_refreshes_share_one_rotation_when_the_vendor_is_still_responding() {
    let (plugin, client, store) = setup(vec![(
        200,
        json!({"accessToken":"next","refreshToken":"rotated","expiresAt":core::iso(core::now()+3600_000),"userInfo":{"clineUserId":"uid-one"}}),
    )]);
    let plugin = Arc::new(plugin);
    let pause = Arc::new(RefreshPause::default());
    *client.pause.lock().unwrap() = Some(pause.clone());
    let mut old = credentials();
    old["expires"] = json!(core::now() + 60_000);
    let first = {
        let (plugin, client, old) = (plugin.clone(), client.clone(), old.clone());
        tokio::spawn(async move { plugin.live(&old, client.as_ref(), true).await })
    };
    pause.entered.notified().await;
    let second = {
        let (plugin, client, old) = (plugin.clone(), client.clone(), old.clone());
        tokio::spawn(async move { plugin.live(&old, client.as_ref(), false).await })
    };
    tokio::task::yield_now().await;
    pause.release.notify_one();
    for result in [first.await.unwrap(), second.await.unwrap()] {
        assert_eq!(result.unwrap()["access"], "next");
    }
    assert_eq!(
        plugin.live(&old, client.as_ref(), true).await.unwrap()["access"],
        "next"
    );
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    assert_eq!(store.saved.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn different_cline_accounts_do_not_reuse_each_others_rotated_pair() {
    let (plugin, client, store) = setup(vec![
        (
            200,
            json!({"accessToken":"next-a","refreshToken":"rotated-a","expiresAt":core::iso(core::now()+3600_000),"userInfo":{"clineUserId":"uid-one"}}),
        ),
        (
            200,
            json!({"accessToken":"next-b","refreshToken":"rotated-b","expiresAt":core::iso(core::now()+3600_000),"userInfo":{"clineUserId":"uid-two"}}),
        ),
    ]);
    let a = credentials();
    let mut b = credentials();
    b["access"] = json!("access-b");
    b["refresh"] = json!("refresh-b");
    b["uid"] = json!("uid-two");
    b["accountId"] = json!("second@example.com");
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
async fn temporary_cline_renewal_failures_use_only_a_still_valid_token_and_never_claim_expiry() {
    for expired in [false, true] {
        let (plugin, client, store) =
            setup(vec![(503, json!({"error":"temporarily_unavailable"}))]);
        let mut old = credentials();
        old["expires"] = json!(if expired { 1 } else { core::now() + 60_000 });
        let result = plugin.live(&old, client.as_ref(), false).await;
        if expired {
            assert_eq!(result.unwrap_err().code, "upstream_transport_error");
        } else {
            assert_eq!(result.unwrap(), old);
        }
        assert!(store.saved.lock().unwrap().is_empty());
    }
    let (plugin, client, store) = setup(vec![(400, json!({"error":"invalid_grant"}))]);
    let mut old = credentials();
    old["expires"] = json!(core::now() + 60_000);
    assert_eq!(
        plugin
            .live(&old, client.as_ref(), false)
            .await
            .unwrap_err()
            .code,
        "authentication_required"
    );
    assert!(store.saved.lock().unwrap().is_empty());
}

#[tokio::test]
async fn repeated_failed_saves_retry_the_same_rotated_pair_without_another_exchange() {
    let (plugin, client, store) = setup(vec![(
        200,
        json!({"accessToken":"next","refreshToken":"rotated","expiresAt":core::iso(core::now()+3600_000),"userInfo":{"clineUserId":"uid-one"}}),
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

fn registered() -> Value {
    json!({"accessToken":"registered-access","refreshToken":"registered-refresh","expiresAt":core::iso(core::now()+3600_000),"userInfo":{"clineUserId":"uid-one"}})
}

async fn begin_device(plugin: &Cline) -> Value {
    let session = plugin
        .core
        .begin(
            json!({"device_code":"fixture-device","interval":1}),
            core::now() + 300_000,
        )
        .await
        .unwrap();
    json!({"session":session})
}

#[tokio::test(start_paused = true)]
async fn a_device_slow_down_increases_every_subsequent_poll_interval() {
    let (plugin, client, _) = setup(vec![
        (400, json!({"error":"slow_down"})),
        (400, json!({"error":"authorization_pending"})),
        (200, json!({"access_token":"approved"})),
        (200, registered()),
    ]);
    let start = tokio::time::Instant::now();
    assert_eq!(
        plugin.callback(begin_device(&plugin).await).await.unwrap()["credentials"]["access"],
        "registered-access"
    );
    let times = client.times.lock().unwrap();
    assert_eq!(times[0] - start, Duration::from_secs(1));
    assert_eq!(times[1] - times[0], Duration::from_secs(2));
    assert_eq!(times[2] - times[1], Duration::from_secs(2));
    assert_eq!(client.requests.lock().unwrap().len(), 4);
}

#[tokio::test(start_paused = true)]
async fn fractional_device_intervals_keep_the_requested_poll_cadence_with_a_one_second_minimum() {
    for (interval, expected) in [(0.25, 1), (1.9, 1), (2.5, 2), (0.0, 5), (-1.0, 5)] {
        let (plugin, client, _) = setup(vec![
            (
                200,
                json!({"device_code":"fixture-device","verification_uri":"https://auth.example.test/approve","interval":interval}),
            ),
            (200, json!({"access_token":"approved"})),
            (200, registered()),
        ]);
        let pending = plugin
            .authorize(json!({"authMethodID":"oauth"}))
            .await
            .unwrap();
        let start = tokio::time::Instant::now();
        plugin.callback(pending).await.unwrap();
        assert_eq!(
            client.times.lock().unwrap()[1] - start,
            Duration::from_secs(expected)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn transient_device_poll_failures_retry_without_losing_the_original_approval() {
    let (plugin, client, _) = setup(vec![
        (503, json!({"_raw":"temporarily unavailable"})),
        (200, json!({"_raw":"not JSON yet"})),
        (0, json!({"_transport_error":true})),
        (
            200,
            json!({"access_token":"approved","refresh_token":"refresh"}),
        ),
        (200, registered()),
    ]);
    assert_eq!(
        plugin.callback(begin_device(&plugin).await).await.unwrap()["credentials"]["access"],
        "registered-access"
    );
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 5);
    assert!(requests[..4].iter().all(|request| request.url == TOKEN));
    assert!(requests[..4].iter().all(|request| String::from_utf8_lossy(
        request.body.as_ref().unwrap()
    )
    .contains("device_code=fixture-device")));
}

#[tokio::test(start_paused = true)]
async fn non_json_device_refusals_stop_immediately_instead_of_polling_until_the_deadline() {
    let (plugin, client, store) = setup(vec![
        (400, json!({"_raw":"<html>bad request</html>"})),
        (200, json!({"access_token":"must-not-approve"})),
        (200, registered()),
    ]);
    assert!(plugin.callback(begin_device(&plugin).await).await.is_err());
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    assert!(store.saved.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn browser_denial_cannot_register_an_account_or_consume_another_pending_login() {
    let (plugin, client, store) = setup(vec![
        (400, json!({"error":"access_denied"})),
        (200, json!({"access_token":"second-approved"})),
        (200, registered()),
    ]);
    let first = begin_device(&plugin).await;
    let second = begin_device(&plugin).await;
    assert!(plugin.callback(first).await.is_err());
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    assert!(store.saved.lock().unwrap().is_empty());
    assert!(plugin.callback(second).await.is_ok());
    assert_eq!(client.requests.lock().unwrap().len(), 3);
}
