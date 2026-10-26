use super::*;
use async_trait::async_trait;
use base64::Engine;
use nora_ai::{HttpClientError, HttpRequest, HttpStreamingResponse};
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex},
};
struct Client {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<(u16, Value)>>,
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
            .expect("Unexpected provider request");
        let bytes = serde_json::to_vec(&value).unwrap();
        Ok(HttpStreamingResponse {
            status,
            headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
            body: Box::pin(futures::stream::once(async move { Ok(bytes.into()) })),
        })
    }
}
struct Store;
#[async_trait]
impl core::CredentialStore for Store {
    async fn save(&self, _: Value) -> PluginResult<()> {
        panic!("Open Design keys do not rotate")
    }
}
fn setup(replies: Vec<(u16, Value)>) -> (OpenDesign, Arc<Client>) {
    let client = Arc::new(Client {
        requests: Mutex::new(Vec::new()),
        replies: Mutex::new(replies.into()),
    });
    let core = Core::with_dependencies("opendesign", client.clone(), Arc::new(Store));
    core.initialize(InitializeParams {
        subscription_accounts_enabled: true,
        installed_manifest: None,
        runtime_config: serde_json::Value::Null,
        api_version: 1,
        plugin_id: "opendesign".into(),
        plugin_directory: PathBuf::new(),
        data_directory: PathBuf::new(),
        manifest: serde_json::from_str(include_str!("../nora-provider.json")).unwrap(),
    })
    .unwrap();
    (OpenDesign { core }, client)
}
fn credential() -> Value {
    json!({"type":"oauth","access":"sk-amr-runtime","control":"vela_ctrl_private","email":"me@example.com"})
}
fn context() -> CallContext {
    CallContext {
        subscription_accounts_enabled: true,
        account: "host-account".into(),
        credentials: Some(credential()),
        network_proxy: None,
        ..Default::default()
    }
}
#[test]
fn separate_control_and_runtime_keys_are_not_interchangeable() {
    assert!(runtime_key(&json!({"key":"vela_ctrl_private"})).is_err());
    assert!(control_key(&json!({"key":"sk-amr-runtime"})).is_err());
    assert_eq!(runtime_key(&credential()).unwrap(), "sk-amr-runtime");
    assert_eq!(control_key(&credential()).unwrap(), "vela_ctrl_private");
}
#[test]
fn device_approval_requires_both_real_keys() {
    assert!(auth_of(&json!({"runtimeKey":"sk-amr-r","user":{"email":"me"}})).is_err());
    let auth=auth_of(&json!({"runtimeKey":"sk-amr-r","controlKey":"vela_ctrl_c","user":{"id":"real-user","email":"real@example.com"}})).unwrap();
    assert_eq!(auth["access"], "sk-amr-r");
    assert_eq!(auth["control"], "vela_ctrl_c");
    assert_eq!(auth["uid"], "real-user");
}
#[tokio::test]
async fn authenticated_model_list_uses_runtime_key_and_real_capabilities() {
    let (plugin, client) = setup(vec![(
        200,
        json!({"data":[{"id":"deepseek-v4-pro","metadata":{"context_limit":12345,"context_budget":500,"capability":"advanced"}},{"id":"new-model","metadata":{"context_limit":1000},"architecture":{"input_modalities":["text","image"]}}]}),
    )]);
    let result = plugin.models(context()).await.unwrap();
    assert_eq!(result["source"], "remote");
    assert_eq!(result["models"][0]["api"], "openai-responses");
    assert_eq!(result["models"][0]["contextWindow"], 12345);
    assert_eq!(result["models"][1]["id"], "new-model");
    assert_eq!(result["models"][1]["input"], json!(["text", "image"]));
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].url, format!("{BASE}/models"));
    assert_eq!(
        requests[0].headers["authorization"],
        "Bearer sk-amr-runtime"
    );
    assert!(!format!("{:?}", requests[0]).contains("vela_ctrl_private"));
}
#[tokio::test]
async fn plan_windows_use_control_key_and_report_actual_reset_and_usage() {
    let (plugin, client) = setup(vec![
        (200, json!({"activeWorkspaceId":"workspace/one"})),
        (
            200,
            json!({"balanceUsd":12.5,"codingPlan":{"eligible":true,"windows":[{"usedCredits":12,"limitCredits":60,"durationSeconds":18000,"resetsAt":"2030-03-17T17:46:40Z"}]}}),
        ),
        (
            200,
            json!({"membershipTier":"pro","subscriptionCancelAtPeriodEnd":true}),
        ),
    ]);
    let result = plugin.usage(context()).await.unwrap();
    assert_eq!(result["items"][0]["usedPercent"], 20.0);
    assert_eq!(result["items"][0]["countsForRouting"], true);
    assert_eq!(result["items"][0]["windowSeconds"], 18000.0);
    assert_eq!(result["balance"], "$12.50");
    assert_eq!(result["renew"], "off");
    let requests = client.requests.lock().unwrap();
    assert!(requests[1].url.contains("workspace%2Fone"));
    assert!(requests
        .iter()
        .all(|r| r.headers["authorization"] == "Bearer vela_ctrl_private"));
}
#[tokio::test]
async fn runtime_only_keys_do_not_fabricate_a_plan_or_start_a_quota_request() {
    let (plugin, client) = setup(vec![]);
    let mut context = context();
    context.credentials = Some(json!({"key":"sk-amr-only","type":"api"}));
    let result = plugin.usage(context).await.unwrap();
    assert_eq!(result["available"], false);
    assert!(client.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn native_tool_calls_and_caller_parameters_are_forwarded_without_rewriting() {
    let (plugin, client) = setup(vec![(
        200,
        json!({"id":"chat","choices":[{"message":{"tool_calls":[{"id":"c","type":"function","function":{"name":"read","arguments":"{}"}}]}}]}),
    )]);
    let body = json!({"model":"new-model","top_p":0.25,"messages":[{"role":"tool","tool_call_id":"c","content":"result"}],"tools":[{"type":"function","function":{"name":"read","parameters":{"type":"object"}}}]});
    let response = plugin
        .fetch(FetchRequest {
            caller_user_agent: None,
            constraints: Default::default(),
            subscription_accounts_enabled: true,
            account: "account".into(),
            credentials: Some(credential()),
            network_proxy: None,
            url: format!("{BASE}/chat/completions"),
            method: "POST".into(),
            headers: BTreeMap::from([("authorization".into(), "Bearer incoming-secret".into())]),
            metadata: BTreeMap::from([("sessionID".into(), "private-host-session".into())]),
            body: Some(
                base64::engine::general_purpose::STANDARD
                    .encode(serde_json::to_vec(&body).unwrap()),
            ),
            body_file: None,
        })
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(requests[0].body.as_ref().unwrap()).unwrap(),
        body
    );
    assert_eq!(
        requests[0].headers["authorization"],
        "Bearer sk-amr-runtime"
    );
    assert!(!format!("{:?}", requests[0]).contains("private-host-session"));
}
#[tokio::test]
async fn vendor_authentication_refusals_are_not_reported_as_success() {
    let (plugin, _) = setup(vec![(401, json!({"error":"denied"}))]);
    assert!(plugin.models(context()).await.is_err());
}

#[tokio::test]
async fn device_sign_in_waits_through_pending_and_incomplete_approval_for_both_keys() {
    let (plugin, client) = setup(vec![
        (
            200,
            json!({"deviceId":"device/one","deviceSecret":"private-device-secret","activationUrl":"https://open-design.ai/activate","userCode":"ABC","pollIntervalSeconds":1,"expiresAt":"2099-01-01T00:00:00Z"}),
        ),
        (
            200,
            json!({"status":"pending","runtimeKey":null,"controlKey":null,"user":null}),
        ),
        (
            200,
            json!({"status":"pending","runtimeKey":"sk-amr-runtime","controlKey":null}),
        ),
        (
            200,
            json!({"status":"approved","runtimeKey":"sk-amr-runtime","controlKey":"vela_ctrl_control","user":{"id":"u1","email":"ada@example.com"}}),
        ),
    ]);
    let begun = plugin
        .authorize(json!({"authMethodID":"oauth"}))
        .await
        .unwrap();
    assert!(begun.get("credentials").is_none());
    assert!(!begun.to_string().contains("private-device-secret"));
    let result = plugin
        .callback(json!({"session":begun["session"]}))
        .await
        .unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["credentials"]["access"], "sk-amr-runtime");
    assert_eq!(result["credentials"]["control"], "vela_ctrl_control");
    assert_eq!(result["credentials"]["uid"], "u1");
    assert_eq!(result["credentials"]["email"], "ada@example.com");
    assert!(plugin
        .callback(json!({"session":begun["session"]}))
        .await
        .is_err());
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests[0].url,
        format!("{CONTROL}/api/v1/cli/device-authorizations")
    );
    for poll in &requests[1..] {
        assert_eq!(poll.method, HttpMethod::Post);
        assert_eq!(
            poll.url,
            format!("{CONTROL}/api/v1/cli/device-authorizations/device%2Fone/token")
        );
        assert!(!poll.headers.contains_key("authorization"));
        assert_eq!(
            serde_json::from_slice::<Value>(poll.body.as_ref().unwrap()).unwrap(),
            json!({"deviceSecret":"private-device-secret"})
        );
    }
}

#[tokio::test]
async fn expired_or_rejected_device_authorizations_stop_without_credentials_or_another_poll() {
    for (status, response) in [
        (200, json!({"status":"expired"})),
        (200, json!({"status":"EXPIRED"})),
        (401, json!({"error":"invalid_device_secret"})),
        (403, json!({"error":"denied"})),
        (404, json!({"error":"not_found"})),
    ] {
        let (plugin, client) = setup(vec![(status, response)]);
        let session = plugin
            .core
            .begin(
                json!({"deviceId":"device","deviceSecret":"secret","interval":0}),
                core::now() + 60_000,
            )
            .await
            .unwrap();
        let error = plugin
            .callback(json!({"session":session}))
            .await
            .unwrap_err();
        assert_eq!(error.code, "authentication_required");
        assert_eq!(client.requests.lock().unwrap().len(), 1);
        assert!(plugin.callback(json!({"session":session})).await.is_err());
        assert_eq!(client.requests.lock().unwrap().len(), 1);
    }
    let (plugin, client) = setup(vec![]);
    let session = plugin
        .core
        .begin(
            json!({"deviceId":"device","deviceSecret":"secret","interval":0}),
            core::now() - 1,
        )
        .await
        .unwrap();
    assert_eq!(
        plugin
            .callback(json!({"session":session}))
            .await
            .unwrap_err()
            .code,
        "authentication_required"
    );
    assert!(client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn actual_string_credit_windows_are_sorted_shortest_first_and_keep_resets_and_balance() {
    let (plugin, client) = setup(vec![
        (200, json!({"activeWorkspaceId":"ws1"})),
        (
            200,
            json!({"balanceUsd":"1.5000","codingPlan":{"eligible":true,"windows":[
                {"durationSeconds":604800,"usedCredits":"16251","limitCredits":"150000","resetsAt":"2026-10-18T06:36:14.000Z"},
                {"durationSeconds":18000,"usedCredits":"16251","limitCredits":"50000","resetsAt":"2026-10-11T11:36:14.000Z"},
                {"durationSeconds":900,"usedCredits":"3","limitCredits":"0"},
                {"durationSeconds":3600,"usedCredits":"NaN","limitCredits":"100"},
                {"durationSeconds":86400,"usedCredits":"10","limitCredits":"Infinity"}
            ]}}),
        ),
        (200, json!({"membershipTier":"go"})),
    ]);
    let usage = plugin.usage(context()).await.unwrap();
    let items = usage["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["label"], "5 hours");
    assert_eq!(items[0]["windowSeconds"], 18000.0);
    assert_eq!(items[0]["usedPercent"], 32.5);
    assert_eq!(items[0]["used"], 16251.0);
    assert_eq!(items[0]["limit"], 50000.0);
    assert_eq!(
        core::timestamp(&items[0]["resetsAt"]),
        core::timestamp(&json!("2026-10-11T11:36:14.000Z"))
    );
    assert_eq!(items[1]["label"], "7 days");
    assert_eq!(items[1]["usedPercent"], 10.8);
    assert_eq!(
        core::timestamp(&items[1]["resetsAt"]),
        core::timestamp(&json!("2026-10-18T06:36:14.000Z"))
    );
    assert_eq!(usage["balance"], "$1.50");
    assert!(items.iter().all(|item| item["countsForRouting"] == true));
    assert!(client
        .requests
        .lock()
        .unwrap()
        .iter()
        .all(|request| request.headers["authorization"] == "Bearer vela_ctrl_private"));
}

#[test]
fn allowance_names_use_minutes_hours_days_and_the_singular_form() {
    for (seconds, label) in [
        (900, "15 minutes"),
        (3600, "1 hour"),
        (86400, "1 day"),
        (0, "Allowance"),
    ] {
        let usage = usage_of(
            &json!({"codingPlan":{"windows":[{"durationSeconds":seconds,"usedCredits":1,"limitCredits":2}]}}),
            &Value::Null,
            "one",
        );
        assert_eq!(usage["items"][0]["label"], label);
    }
}
