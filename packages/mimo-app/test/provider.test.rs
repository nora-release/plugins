use super::*;
use crate::test_support::{raw, reply, Fixture};
use base64::Engine;
use nora_ai::{HttpClient, HttpClientError, HttpRequest, HttpStreamingResponse};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex as StdMutex,
    },
};
struct NoHttp;
#[async_trait]
impl HttpClient for NoHttp {
    async fn stream(&self, _: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        panic!("MiMo must use the transport that preserves duplicate Set-Cookie headers")
    }
}
struct Store {
    saved: StdMutex<Vec<Value>>,
    failures: AtomicUsize,
}
#[async_trait]
impl core::CredentialStore for Store {
    async fn save(&self, value: Value) -> PluginResult<()> {
        self.saved.lock().unwrap().push(value);
        if self
            .failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
                count.checked_sub(1)
            })
            .is_ok()
        {
            return Err(PluginError::transport("Save failed"));
        }
        Ok(())
    }
}
fn setup(replies: Vec<crate::test_support::Response>) -> (Mimo, Arc<Fixture>, Arc<Store>) {
    let fixture = Fixture::new(replies);
    let store = Arc::new(Store {
        saved: StdMutex::new(Vec::new()),
        failures: AtomicUsize::new(0),
    });
    let mut plugin = Mimo::default();
    plugin.net = fixture.clone();
    plugin.core = Core::with_dependencies("mimo-app", Arc::new(NoHttp), store.clone());
    plugin
        .core
        .initialize(InitializeParams {
            subscription_accounts_enabled: true,
            installed_manifest: None,
            runtime_config: serde_json::Value::Null,
            api_version: 2,
            plugin_id: "mimo-app".into(),
            plugin_directory: PathBuf::new(),
            data_directory: PathBuf::new(),
            manifest: serde_json::from_str(include_str!("../nora-plugin.json")).unwrap(),
        })
        .unwrap();
    (plugin, fixture, store)
}
fn credentials() -> Value {
    auth::credential(
        json!({"userId":"one","passToken":"private-pass","cUserId":"cipher","deviceId":"pc_device","region":"SGP"}),
        BTreeMap::from([
            ("serviceToken".into(), "old-session".into()),
            ("userId".into(), "one".into()),
        ]),
        &json!({"nickname":"Name"}),
    )
}
fn request() -> FetchRequest {
    FetchRequest{ caller_user_agent: None, constraints: Default::default(), subscription_accounts_enabled: true,account:"one".into(),credentials:Some(credentials()),network_proxy:None,url:format!("{}/route/chat/completions",auth::base("SGP").unwrap()),method:"POST".into(),headers:BTreeMap::from([("authorization".into(),"Bearer untrusted".into()),("cookie".into(),"untrusted=one".into())]),metadata:BTreeMap::new(),body:Some(base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&json!({"model":"mimo-pro","messages":[{"role":"user","content":"hello"}],"top_p":0.4,"stop":["END"],"tools":[{"type":"function","function":{"name":"read","parameters":{"type":"object"}}}]})).unwrap())),body_file:None}
}
async fn body(response: FetchResponse) -> Value {
    use futures::StreamExt;
    let bytes = response
        .body
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<PluginResult<Vec<_>>>()
        .unwrap()
        .concat();
    serde_json::from_slice(&bytes).unwrap()
}
fn renewed() -> crate::test_support::Response {
    reply(
        "/user/xiaomi/me",
        200,
        json!({"code":0,"data":{"userId":"one","region":"SGP"}}),
    )
    .cookie("serviceToken=new-session; Path=/; Secure")
    .cookie("userId=one; Path=/; Secure")
}
async fn platform_cookie(plugin: &Mimo) {
    plugin.platform.lock().await.insert(
        "one".into(),
        (
            core::now(),
            BTreeMap::from([("serviceToken".into(), "platform-session".into())]),
        ),
    );
}
fn token_plan_replies() -> Vec<crate::test_support::Response> {
    vec![
        reply(
            "/api/v1/tokenPlan/detail",
            200,
            json!({"code":0,"data":{"planName":"Pro","expired":false}}),
        ),
        reply(
            "/api/v1/tokenPlan/apiKey/raw",
            200,
            json!({"code":0,"data":"tp-real-key"}),
        ),
        reply(
            "/api/v1/tokenPlan/apiKey",
            200,
            json!({"code":0,"data":{"openaiBaseUrl":"https://token-plan-sgp.xiaomimimo.com/v1"}}),
        ),
    ]
}
#[tokio::test]
async fn session_renewal_waits_for_ack_and_is_reused_after_a_failed_ack() {
    let (plugin, fixture, store) = setup(vec![renewed()]);
    store.failures.store(1, Ordering::SeqCst);
    let mut old = credentials();
    old["expires"] = json!(core::now() - 1);
    assert!(plugin.fresh(&old, None, false).await.is_err());
    let next = plugin.fresh(&old, None, false).await.unwrap();
    assert_eq!(next["cookies"]["serviceToken"], "new-session");
    assert_eq!(store.saved.lock().unwrap().len(), 2);
    assert_eq!(fixture.requests.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn renewal_rejects_an_account_switch_and_transient_failures_do_not_delete_the_original_session(
) {
    let (plugin, _, store) = setup(vec![reply(
        "/user/xiaomi/me",
        200,
        json!({"code":0,"data":{"userId":"other"}}),
    )
    .cookie("serviceToken=wrong; Path=/; Secure")]);
    assert_eq!(
        plugin
            .fresh(&credentials(), None, true)
            .await
            .unwrap_err()
            .code,
        "authentication_required"
    );
    assert!(store.saved.lock().unwrap().is_empty());
    let (plugin, _, store) = setup(vec![reply("/user/xiaomi/me", 403, json!({"code":46109}))]);
    let mut stored = credentials();
    stored["expires"] = json!(core::now() - 1);
    let got = plugin.fresh(&stored, None, false).await.unwrap();
    assert_eq!(got["cookies"]["serviceToken"], "old-session");
    assert!(store.saved.lock().unwrap().is_empty());
}
#[tokio::test]
async fn native_app_fetch_preserves_tools_and_sampling_without_forwarding_authorization_or_pass_tokens(
) {
    let (plugin, fixture, _) = setup(vec![reply(
        "/route/chat/completions",
        200,
        json!({"choices":[{"message":{"content":"actual"}}]}),
    )]);
    let request = request();
    let original = request.decode_body().unwrap().unwrap();
    let response = body(plugin.fetch(request).await.unwrap()).await;
    assert_eq!(response["choices"][0]["message"]["content"], "actual");
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(
        requests[0].request.body.as_ref().unwrap().as_ref(),
        original.as_slice()
    );
    assert_eq!(
        requests[0].request.headers["x-mimo-source"],
        "mimocode-cli-free"
    );
    assert!(!requests[0].request.headers.contains_key("authorization"));
    assert!(requests[0].request.headers["cookie"].contains("old-session"));
    assert!(!requests[0]
        .request
        .headers
        .values()
        .any(|value| value.contains("private-pass")));
}
#[tokio::test]
async fn app_membership_refusal_uses_only_an_existing_token_plan_key_and_stays_on_that_account() {
    let mut replies = vec![reply(
        "/api/route/chat/completions",
        403,
        json!({"error":{"code":"membership_required"}}),
    )];
    replies.extend(token_plan_replies());
    replies.extend([
        reply(
            "token-plan-sgp.xiaomimimo.com/v1/chat/completions",
            200,
            json!({"choices":[{"message":{"content":"plan"}}]}),
        ),
        reply(
            "token-plan-sgp.xiaomimimo.com/v1/chat/completions",
            200,
            json!({"choices":[{"message":{"content":"again"}}]}),
        ),
    ]);
    let (plugin, fixture, _) = setup(replies);
    platform_cookie(&plugin).await;
    assert_eq!(
        body(plugin.fetch(request()).await.unwrap()).await["choices"][0]["message"]["content"],
        "plan"
    );
    assert_eq!(
        body(plugin.fetch(request()).await.unwrap()).await["choices"][0]["message"]["content"],
        "again"
    );
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 6);
    let plan = requests
        .iter()
        .find(|record| record.request.url.contains("token-plan-sgp"))
        .unwrap();
    let payload: Value = serde_json::from_slice(plan.request.body.as_ref().unwrap()).unwrap();
    assert_eq!(payload["model"], "mimo-v2.6-pro");
    assert_eq!(payload["top_p"], 0.4);
    assert_eq!(payload["stop"], json!(["END"]));
    assert_eq!(payload["tools"][0]["function"]["name"], "read");
    assert_eq!(plan.request.headers["authorization"], "Bearer tp-real-key");
    assert!(!plan.request.headers.contains_key("cookie"));
    assert!(!plan.request.headers.contains_key("x-mimo-source"));
    assert!(requests
        .iter()
        .filter(|record| record.request.url.contains("platform.xiaomimimo.com"))
        .all(|record| record.request.method == HttpMethod::Get));
}
#[tokio::test]
async fn an_expired_or_missing_token_plan_preserves_the_original_membership_error() {
    let (plugin, fixture, _) = setup(vec![
        reply(
            "/route/chat/completions",
            403,
            json!({"error":{"code":"membership_required","message":"Need membership"}}),
        ),
        reply(
            "/api/v1/tokenPlan/detail",
            200,
            json!({"code":0,"data":{"planName":"old","expired":true}}),
        ),
    ]);
    platform_cookie(&plugin).await;
    let result = plugin.fetch(request()).await.unwrap();
    assert_eq!(result.status, 403);
    assert_eq!(body(result).await["error"]["message"], "Need membership");
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn a_reset_token_plan_key_is_invalidated_instead_of_switching_to_another_account() {
    let mut replies = vec![reply(
        "/route/chat/completions",
        403,
        json!({"error":{"code":"membership_required"}}),
    )];
    replies.extend(token_plan_replies());
    replies.push(reply(
        "token-plan-sgp.xiaomimimo.com/v1/chat/completions",
        401,
        json!({"error":"reset key"}),
    ));
    let (plugin, _, _) = setup(replies);
    platform_cookie(&plugin).await;
    assert_eq!(plugin.fetch(request()).await.unwrap().status, 401);
    assert!(!plugin.keys.lock().await.contains_key("one"));
}
#[tokio::test]
async fn usage_separates_the_app_week_from_token_plan_api_credits() {
    let (plugin, fixture, _) = setup(vec![
        reply(
            "/user/xiaomi/subscription/self",
            200,
            json!({"code":0,"data":{"current":{"title":"App Pro","endTime":"2026-11-01","renewalMode":"MONTHLY"}}}),
        ),
        reply(
            "/api/v1/tokenPlan/detail",
            200,
            json!({"code":0,"data":{"planName":"API Pro","expired":false,"currentPeriodEnd":"2026-11-01 00:00:00"}}),
        ),
        reply(
            "/api/v1/tokenPlan/usage",
            200,
            json!({"code":0,"data":{"monthUsage":{"items":[{"name":"month_total_token","used":25,"limit":100}]}}}),
        ),
        reply(
            "/user/usage",
            200,
            json!({"code":0,"data":{"percent":80,"resetDate":"2026-10-29"}}),
        ),
    ]);
    platform_cookie(&plugin).await;
    let value = plugin
        .usage(CallContext {
            subscription_accounts_enabled: true,
            account: "one".into(),
            credentials: Some(credentials()),
            network_proxy: None,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(value["plan"], "App Pro");
    assert_eq!(value["items"][0]["usedPercent"], 20.0);
    assert_eq!(value["items"][1]["countsForRouting"], false);
    assert_eq!(value["items"][1]["usedPercent"], 25.0);
    assert_eq!(fixture.requests.lock().unwrap().len(), 4);
}
#[tokio::test]
async fn a_stale_account_page_reauthenticates_once_and_keeps_the_verified_new_session() {
    let (plugin, fixture, store) = setup(vec![
        raw(
            "/user/xiaomi/subscription/self",
            200,
            b"<html>sign in</html>".to_vec(),
        ),
        renewed(),
        reply(
            "/user/xiaomi/subscription/self",
            200,
            json!({"code":0,"data":{"current":null}}),
        ),
    ]);
    let value = plugin
        .page(&credentials(), "/user/xiaomi/subscription/self", None)
        .await
        .unwrap();
    assert!(value["current"].is_null());
    assert_eq!(fixture.requests.lock().unwrap().len(), 3);
    assert_eq!(store.saved.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn model_presets_are_honest_and_auto_alias_changes_only_the_top_level_model() {
    let (plugin, fixture, _) = setup(vec![reply(
        "/route/chat/completions",
        200,
        json!({"choices":[]}),
    )]);
    let models = plugin
        .models(CallContext {
            subscription_accounts_enabled: true,
            account: String::new(),
            credentials: None,
            network_proxy: None,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(models["source"], "preset");
    assert_eq!(models["models"].as_array().unwrap().len(), 2);
    assert_eq!(models["models"][0]["contextWindow"], 1_000_000);
    let mut request = request();
    request.body=Some(base64::engine::general_purpose::STANDARD.encode(br#"{"model":"mimo-auto","messages":[{"role":"user","content":"mimo-auto remains quoted content"}]}"#));
    let _ = body(plugin.fetch(request).await.unwrap()).await;
    let requests = fixture.requests.lock().unwrap();
    let payload: Value =
        serde_json::from_slice(requests[0].request.body.as_ref().unwrap()).unwrap();
    assert_eq!(payload["model"], "mimo-pro");
    assert_eq!(
        payload["messages"][0]["content"],
        "mimo-auto remains quoted content"
    );
}
#[tokio::test]
async fn explicit_proxy_settings_reach_the_transport_without_becoming_vendor_headers() {
    let (plugin, fixture, _) = setup(vec![reply(
        "/route/chat/completions",
        200,
        json!({"choices":[]}),
    )]);
    let mut request = request();
    let proxy = NetworkProxy::Proxy {
        url: "http://proxy-user:proxy-secret@127.0.0.1:8888".into(),
    };
    request.network_proxy = Some(proxy.clone());
    let _ = body(plugin.fetch(request).await.unwrap()).await;
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests[0].proxy, Some(proxy));
    assert!(!requests[0]
        .request
        .headers
        .values()
        .any(|value| value.contains("proxy-secret")));
    assert_eq!(requests[0].timeout, Duration::from_secs(60));
}

#[tokio::test]
async fn a_verified_body_file_preserves_native_tools_and_sampling() {
    let (plugin, fixture, _) = setup(vec![reply(
        "/route/chat/completions",
        200,
        json!({"choices":[]}),
    )]);
    let mut request = request();
    let expected = request.decode_body().unwrap().unwrap();
    let path = std::env::temp_dir().join(format!(
        "nora-mimo-body-{}-{}.json",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::write(&path, &expected).unwrap();
    request.body = None;
    request.body_file = Some(nora_provider_plugin_sdk::RequestBodyFile::from_path(&path).unwrap());
    let _ = body(plugin.fetch(request).await.unwrap()).await;
    std::fs::remove_file(path).unwrap();
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(
        requests[0].request.body.as_deref(),
        Some(expected.as_slice())
    );
}

#[tokio::test]
async fn oauth_callback_returns_credentials_only_after_the_actual_mimo_session_is_verified() {
    let (plugin, fixture, _) = setup(vec![
        raw("/user/xiaomi/me", 302, Vec::new()).header(
            "location",
            "https://account.xiaomi.com/pass/serviceLogin?sid=mimosgp",
        ),
        raw(
            "/longPolling/loginUrl?",
            200,
            br#"&&&START&&&{"code":0,"loginUrl":"https://account.xiaomi.com/qr","lp":"https://account.xiaomi.com/poll/ticket","timeout":60}"#.to_vec(),
        ),
        raw(
            "/poll/ticket",
            200,
            br#"&&&START&&&{"userId":"one","passToken":"private-pass","cUserId":"cipher"}"#.to_vec(),
        ),
        renewed(),
    ]);
    let authorization = plugin
        .authorize(json!({"authMethodID":"oauth"}))
        .await
        .unwrap();
    assert_eq!(authorization["url"], "https://account.xiaomi.com/qr");
    assert!(authorization.get("credentials").is_none());
    let completed = plugin
        .callback(json!({"session":authorization["session"]}))
        .await
        .unwrap();
    assert_eq!(completed["ok"], true);
    assert_eq!(completed["credentials"]["uid"], "one");
    assert_eq!(
        completed["credentials"]["cookies"]["serviceToken"],
        "new-session"
    );
    assert_eq!(fixture.requests.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn a_busy_or_unavailable_session_service_keeps_existing_credentials_without_acknowledging_fake_updates(
) {
    for status in [429, 503] {
        let (plugin, _, store) = setup(vec![raw("/user/xiaomi/me", status, b"Try later".to_vec())]);
        let mut old = credentials();
        old["expires"] = json!(core::now() - 1);
        let kept = plugin.fresh(&old, None, false).await.unwrap();
        assert_eq!(kept, old);
        assert!(store.saved.lock().unwrap().is_empty());
    }
}
