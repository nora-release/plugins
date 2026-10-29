use super::*;
use base64::Engine;
use nora_ai::{HttpClientError, HttpRequest};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex as StdMutex,
    },
};
struct Client {
    requests: StdMutex<Vec<HttpRequest>>,
    replies: StdMutex<VecDeque<(u16, Value)>>,
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
            .expect("Unexpected Raccoon request");
        let bytes = serde_json::to_vec(&value).unwrap();
        Ok(HttpStreamingResponse {
            status,
            headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
            body: Box::pin(futures::stream::once(async move { Ok(bytes.into()) })),
        })
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
fn setup(replies: Vec<(u16, Value)>) -> (Raccoon, Arc<Client>, Arc<Store>) {
    let client = Arc::new(Client {
        requests: StdMutex::new(Vec::new()),
        replies: StdMutex::new(replies.into()),
    });
    let store = Arc::new(Store {
        saved: StdMutex::new(Vec::new()),
        failures: AtomicUsize::new(0),
    });
    let core = Core::with_dependencies("raccoon", client.clone(), store.clone());
    core.initialize(InitializeParams {
        subscription_accounts_enabled: true,
        installed_manifest: None,
        runtime_config: serde_json::Value::Null,
        api_version: 2,
        plugin_id: "raccoon".into(),
        plugin_directory: PathBuf::new(),
        data_directory: PathBuf::new(),
        manifest: serde_json::from_str(include_str!("../nora-plugin.json")).unwrap(),
    })
    .unwrap();
    (
        Raccoon {
            core,
            browsers: Mutex::new(BTreeMap::new()),
            onboarding: Mutex::new(BTreeSet::new()),
        },
        client,
        store,
    )
}
fn token(uid: &str, marker: &str) -> String {
    format!(
        "e30.{}.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({"sid":uid,"name":"Account","exp":core::now()/1000+3600}))
                .unwrap()
        ),
        marker
    )
}
fn credentials() -> Value {
    let mut value = auth::credential(&token("ff", "old"), "old-refresh", "oauth").unwrap();
    value["accountId"] = json!("original-account");
    value
}
fn ok(value: Value) -> (u16, Value) {
    (200, json!({"code":0,"data":value}))
}
#[tokio::test]
async fn rotated_tokens_wait_for_ack_and_are_not_refreshed_twice() {
    let new = token("ff", "new");
    let (plugin, client, store) = setup(vec![ok(
        json!({"access_token":new,"refresh_token":"rotated"}),
    )]);
    store.failures.store(1, Ordering::SeqCst);
    let old = credentials();
    assert!(plugin.live(&old, client.as_ref(), true).await.is_err());
    let refreshed = plugin.live(&old, client.as_ref(), true).await.unwrap();
    assert_eq!(refreshed["access"], new);
    assert_eq!(refreshed["accountId"], "original-account");
    assert_eq!(refreshed["refresh"], "rotated");
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    assert_eq!(store.saved.lock().unwrap().len(), 2);
    let sent: Value =
        serde_json::from_slice(client.requests.lock().unwrap()[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(sent, json!({"refresh_token":"old-refresh"}));
}
#[tokio::test]
async fn refresh_rejects_an_account_switch_without_saving_it() {
    let (plugin, client, store) = setup(vec![ok(
        json!({"access_token":token("01","other"),"refresh_token":"other-refresh"}),
    )]);
    assert_eq!(
        plugin
            .live(&credentials(), client.as_ref(), true)
            .await
            .unwrap_err()
            .code,
        "authentication_required"
    );
    assert!(store.saved.lock().unwrap().is_empty());
}
#[tokio::test]
async fn transient_refresh_errors_keep_the_account_and_do_not_claim_expiry() {
    let (plugin, client, store) = setup(vec![(429, json!({"message":"busy"}))]);
    assert_eq!(
        plugin
            .live(&credentials(), client.as_ref(), true)
            .await
            .unwrap_err()
            .code,
        "upstream_transport_error"
    );
    assert!(store.saved.lock().unwrap().is_empty());
}
#[tokio::test]
async fn explicit_desktop_source_saves_only_its_binding_and_detects_later_account_changes() {
    let path = std::env::temp_dir().join(format!("raccoon-provider-{}.json", auth::state()));
    let original = serde_json::to_vec(
        &json!({"access_token":token("ff","desktop"),"refresh_token":"never-consume"}),
    )
    .unwrap();
    std::fs::write(&path, &original).unwrap();
    let (plugin, client, store) = setup(vec![ok(json!({"available_points":10}))]);
    let result = plugin
        .authorize(json!({"authMethodID":"desktop","inputs":{"sourcePath":path}}))
        .await
        .unwrap();
    let saved = &result["credentials"];
    assert!(saved.get("access").is_none());
    assert!(saved.get("refresh").is_none());
    assert_eq!(saved["uid"], "255");
    assert_eq!(std::fs::read(&path).unwrap(), original);
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({"access_token":token("01","switched")})).unwrap(),
    )
    .unwrap();
    assert!(plugin.live(saved, client.as_ref(), false).await.is_err());
    assert!(store.saved.lock().unwrap().is_empty());
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    std::fs::remove_file(path).unwrap();
}
#[tokio::test]
async fn manual_state_errors_do_not_consume_pending_signin_and_success_requires_vendor_validation()
{
    let (plugin, client, _) = setup(vec![
        ok(json!({"access_token":token("ff","approved"),"refresh_token":"real-refresh"})),
        ok(json!({"available_points":10})),
    ]);
    let start = plugin
        .authorize(json!({"authMethodID":"manual"}))
        .await
        .unwrap();
    let session = start["session"].as_str().unwrap();
    let state = plugin.core.pending.lock().await[session]["state"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(plugin
        .callback(
            json!({"session":session,"code":"office-raccoon://auth/callback?code=one&state=wrong"})
        )
        .await
        .is_err());
    assert!(plugin.core.pending.lock().await.contains_key(session));
    let result=plugin.callback(json!({"session":session,"code":format!("office-raccoon://auth/callback?code=real-code&state={state}")})).await.unwrap();
    assert_eq!(result["credentials"]["refresh"], "real-refresh");
    assert!(!plugin.core.pending.lock().await.contains_key(session));
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].url.ends_with("login_with_authorization_code"));
    assert_eq!(
        serde_json::from_slice::<Value>(requests[0].body.as_ref().unwrap()).unwrap()
            ["authorization_code"],
        "real-code"
    );
    assert!(requests[1].url.ends_with("points/v1/balance"));
}
#[tokio::test]
async fn live_catalog_preserves_native_ids_and_relative_credit_multipliers() {
    let (plugin, client, _) = setup(vec![ok(
        json!({"categories":[{"models":[{"name":"actual-live-id","display_name":"Live model","billing_effective_multiplier":0.25,"billing_multiplier":1}]}]}),
    )]);
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
    let model = result["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["id"] == "actual-live-id")
        .unwrap();
    assert_eq!(model["info"]["creditMultiplier"], 0.25);
    assert_eq!(model["info"]["previousCreditMultiplier"], 1.0);
    assert!(model["info"].get("requestCreditRate").is_none());
    assert_eq!(result["source"], "remote");
    assert!(client.requests.lock().unwrap()[0]
        .url
        .ends_with("model_catalog"));
}
#[tokio::test]
async fn usage_reads_a_larger_real_bill_page_only_when_the_first_is_full() {
    let first = vec![json!({"points":-1}); 1000];
    let (plugin, client, _) = setup(vec![
        ok(json!({"available_points":50})),
        ok(json!({})),
        ok(json!({"items":first})),
        ok(json!({"items":[{"points":100}]})),
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
    assert_eq!(result["items"][0]["used"], 50.0);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[3].url.contains("paging.limit=5000"));
}
#[tokio::test]
async fn native_fetch_preserves_tools_and_options_while_restoring_the_requested_response_model() {
    let (plugin, client, _) = setup(vec![(
        200,
        json!({"model":"upstream-alias","choices":[{"message":{"tool_calls":[{"id":"call","function":{"name":"read","arguments":"{}"}}]}}]}),
    )]);
    let body = json!({"model":"chosen-id","messages":[{"role":"user","content":"hello"}],"tools":[{"type":"function","function":{"name":"read","parameters":{"type":"object"}}}],"top_p":0.4,"stop":["END"]});
    let bytes = serde_json::to_vec(&body).unwrap();
    let request = FetchRequest {
        caller_user_agent: None,
        constraints: Default::default(),
        subscription_accounts_enabled: true,
        account: "one".into(),
        credentials: Some(credentials()),
        network_proxy: None,
        url: format!("{}/chat/completions", auth::BASE),
        method: "POST".into(),
        headers: BTreeMap::from([("authorization".into(), "Bearer untrusted".into())]),
        metadata: BTreeMap::from([("sessionID".into(), "private-session".into())]),
        body: Some(base64::engine::general_purpose::STANDARD.encode(&bytes)),
        body_file: None,
    };
    let response = plugin.fetch(request).await.unwrap();
    let output = response
        .body
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<PluginResult<Vec<_>>>()
        .unwrap()
        .concat();
    let result: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(result["model"], "chosen-id");
    assert_eq!(
        result["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "read"
    );
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].body.as_ref().unwrap(), &bytes);
    assert_ne!(requests[0].headers["authorization"], "Bearer untrusted");
    assert!(!requests[0]
        .headers
        .values()
        .any(|value| value.contains("private-session")));
}
#[tokio::test]
async fn checkin_counts_new_bills_once_and_persists_onboarding_before_returning() {
    let day = readings::today();
    let old = json!({"bill_id":"old","points":5,"biz_type":"daily_grant","created_at":format!("{day} 01:00:00")});
    let reward = json!({"bill_id":"reward","points":30,"biz_type":"reward_grant","created_at":format!("{day} 02:00:00")});
    let daily = json!({"bill_id":"daily","points":10,"biz_type":"daily_grant","created_at":format!("{day} 02:00:00")});
    let (plugin, client, store) = setup(vec![
        ok(json!({"items":[old.clone()]})),
        ok(json!({})),
        ok(json!({"granted":true,"popup":{"points":30}})),
        ok(json!({"items":[old.clone(),reward.clone(),daily.clone()]})),
        ok(json!({"items":[old.clone(),reward.clone(),daily.clone()]})),
        ok(json!({})),
        ok(json!({"items":[old,reward,daily]})),
    ]);
    let saved = credentials();
    let first = plugin.checkin(client.as_ref(), &saved).await.unwrap();
    assert_eq!(first["outcome"], "claimed");
    assert_eq!(first["credit"], 40.0);
    let updated = store.saved.lock().unwrap().last().unwrap().clone();
    assert_eq!(updated["onboardingSettled"], true);
    let second = plugin.checkin(client.as_ref(), &updated).await.unwrap();
    assert_eq!(second["outcome"], "done");
    assert_eq!(client.requests.lock().unwrap().len(), 7);
}
#[tokio::test]
async fn checkin_never_invents_a_reward_when_the_before_snapshot_failed() {
    let (plugin, client, _) = setup(vec![
        (503, json!({})),
        ok(json!({})),
        ok(json!({"granted":false})),
    ]);
    let result = plugin
        .checkin(client.as_ref(), &credentials())
        .await
        .unwrap();
    assert_eq!(result["outcome"], "failed");
    assert!(result.get("credit").is_none());
}
