use super::*;

const RAW: &str = "glm-limited-preview";
const TRIAL: &str = "glm-limited-preview-Trial";
const BALANCE: &str = "/api/v1/zcode-plan/billing/balance?app_version=3.14.3";

fn balance(left: u64) -> Value {
    json!({"server_time":1000,"plans":[{"plan_id":"grant","user_plan_id":"g1","name":"Gift","status":"active","ends_at":4600}],"balances":[{"plan_id":"grant","user_plan_id":"g1","show_name":"Limited credit","capabilities":[format!("model:{RAW}")],"total_units":100,"used_units":100-left,"remaining_units":left,"expires_at":4600}]})
}
fn plans() -> Reply {
    reply(
        "/api/biz/subscription/list",
        json!([{"productName":"GLM Coding Pro","status":"VALID"}]),
    )
}
fn context(stored: &Value) -> CallContext {
    CallContext {
        subscription_accounts_enabled: true,
        account: "one".into(),
        credentials: Some(stored.clone()),
        network_proxy: None,
        ..Default::default()
    }
}
fn queue_usage(client: &Client, value: Value, paid: bool) {
    let mut replies = client.replies.lock().unwrap();
    if paid {
        replies.extend([
            reply(
                "/api/monitor/usage/quota/limit",
                json!({"limits":[{"unit":3,"usage":100,"remaining":75}]}),
            ),
            plans(),
            reply(
                "/api/biz/customer-package-reset/list?targetType=PERSONAL",
                json!({}),
            ),
        ]);
    }
    replies.extend([
        reply(BALANCE, value),
        reply(
            format!(
                "/api/v1/zcode-plan/billing/preview?app_version=3.14.3&platform={}",
                endpoint::platform()
            ),
            json!({"plans":[]}),
        ),
    ]);
}
async fn catalog_without_network(plugin: &Zcode) {
    *plugin.config.lock().await = Some((core::now(), json!({})));
}
fn has_model(listing: &Value, id: &str) -> bool {
    listing["models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|model| model["id"] == id)
}
fn messages(client: &Client) -> Vec<(String, Value)> {
    client
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.url.ends_with("/messages"))
        .map(|request| {
            (
                request.url.clone(),
                serde_json::from_slice(request.body.as_ref().unwrap()).unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn spent_and_replenished_gift_cache_stays_discoverable_and_never_uses_paid_fallback() {
    let (plugin, client, _) = setup(vec![plans(), reply(BALANCE, balance(75))]);
    catalog_without_network(&plugin).await;
    let stored = credentials("key.secret", &jwt());
    assert!(has_model(
        &plugin.models(context(&stored)).await.unwrap(),
        TRIAL
    ));
    queue_usage(&client, balance(0), true);
    let spent = plugin.usage(context(&stored)).await.unwrap();
    assert_eq!(spent["items"][1]["usedPercent"], 100.0);
    assert_eq!(spent["items"][0]["notModels"], json!([TRIAL]));
    let count = client.requests.lock().unwrap().len();
    assert!(has_model(
        &plugin.models(context(&stored)).await.unwrap(),
        TRIAL
    ));
    assert_eq!(
        plugin
            .fetch(request(stored.clone(), TRIAL))
            .await
            .unwrap()
            .status,
        429
    );
    assert_eq!(client.requests.lock().unwrap().len(), count);
    queue_usage(&client, balance(50), true);
    let renewed = plugin.usage(context(&stored)).await.unwrap();
    assert_eq!(renewed["items"][1]["usedPercent"], 50.0);
    client.replies.lock().unwrap().push_back(Reply {
        path: "/api/v1/zcode-plan/anthropic/v1/messages".into(),
        method: HttpMethod::Post,
        status: 200,
        value: json!({"type":"message","content":[]}),
    });
    assert_eq!(
        plugin.fetch(request(stored, TRIAL)).await.unwrap().status,
        200
    );
    let sent = messages(&client);
    assert_eq!(sent.len(), 1);
    assert!(sent[0].0.starts_with("https://zcode.z.ai/"));
    assert_eq!(sent[0].1["model"], RAW);
    assert!(client.replies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_grant_expiring_before_route_ttl_is_reloaded_and_removed_on_the_next_model_read() {
    let mut initial = balance(75);
    initial["plans"][0]["ends_at"] = json!(1030);
    let (plugin, client, _) = setup(vec![plans(), reply(BALANCE, initial.clone())]);
    catalog_without_network(&plugin).await;
    let stored = credentials("key.secret", &jwt());
    assert!(has_model(
        &plugin.models(context(&stored)).await.unwrap(),
        TRIAL
    ));
    let key = route_key(&stored["state"]);
    {
        let mut routes = plugin.routes.lock().await;
        let cached = routes.get_mut(&key).unwrap();
        assert!(cached.ttl <= 30_000);
        cached.at = core::now().saturating_sub(31_000);
    }
    initial["server_time"] = json!(1031);
    client
        .replies
        .lock()
        .unwrap()
        .extend([plans(), reply(BALANCE, initial)]);
    assert!(!has_model(
        &plugin.models(context(&stored)).await.unwrap(),
        TRIAL
    ));
    assert!(messages(&client).is_empty());
    assert!(client.replies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn usage_refresh_switches_to_a_live_sibling_without_duplicate_or_renamed_models() {
    let mut first = balance(75);
    first["balances"][0]["capabilities"] = json!([format!("model:{}", RAW.to_uppercase())]);
    let mut sibling = first["balances"][0].clone();
    sibling["capabilities"] = json!([format!("model:{RAW}")]);
    sibling["show_name"] = json!("Sibling");
    sibling["remaining_units"] = json!(0);
    sibling["used_units"] = json!(100);
    first["balances"].as_array_mut().unwrap().push(sibling);
    let (plugin, client, _) = setup(vec![plans(), reply(BALANCE, first.clone())]);
    catalog_without_network(&plugin).await;
    let stored = credentials("key.secret", &jwt());
    let expected = format!("{}-Trial", RAW.to_uppercase());
    let before = plugin.models(context(&stored)).await.unwrap();
    assert!(has_model(&before, &expected));
    first["balances"][0]["remaining_units"] = json!(0);
    first["balances"][0]["used_units"] = json!(100);
    first["balances"][1]["remaining_units"] = json!(60);
    first["balances"][1]["used_units"] = json!(40);
    queue_usage(&client, first, true);
    let usage = plugin.usage(context(&stored)).await.unwrap();
    assert_eq!(usage["items"][1]["countsForRouting"], false);
    assert_eq!(usage["items"][2]["countsForRouting"], true);
    assert_eq!(usage["items"][2]["usedPercent"], 40.0);
    let after = plugin.models(context(&stored)).await.unwrap();
    assert_eq!(before, after);
    assert_eq!(
        after["models"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|model| model["id"].as_str().unwrap().eq_ignore_ascii_case(TRIAL))
            .count(),
        1
    );
    client.replies.lock().unwrap().push_back(Reply {
        path: "/api/v1/zcode-plan/anthropic/v1/messages".into(),
        method: HttpMethod::Post,
        status: 200,
        value: json!({"type":"message","content":[]}),
    });
    assert_eq!(
        plugin
            .fetch(request(stored, &expected))
            .await
            .unwrap()
            .status,
        200
    );
    assert!(messages(&client)[0].1["model"]
        .as_str()
        .unwrap()
        .eq_ignore_ascii_case(RAW));
}

#[tokio::test]
async fn gift_only_accounts_keep_real_and_trial_models_routable_when_a_sibling_is_spent() {
    let mut value = balance(0);
    let mut live = value["balances"][0].clone();
    live["show_name"] = json!("Trust Build");
    live["remaining_units"] = json!(85);
    live["used_units"] = json!(15);
    value["balances"].as_array_mut().unwrap().push(live);
    let (plugin, client, _) = setup(vec![reply(BALANCE, value.clone())]);
    catalog_without_network(&plugin).await;
    let stored = credentials("", &jwt());
    let listing = plugin.models(context(&stored)).await.unwrap();
    assert!(has_model(&listing, RAW));
    assert!(has_model(&listing, TRIAL));
    queue_usage(&client, value.clone(), false);
    let usage = plugin.usage(context(&stored)).await.unwrap();
    assert_eq!(usage["items"][0]["countsForRouting"], false);
    assert_eq!(usage["items"][1]["models"], json!([RAW, TRIAL]));
    assert_eq!(usage["items"][1]["usedPercent"], 15.0);
    client.replies.lock().unwrap().extend([
        reply(BALANCE, value),
        Reply {
            path: "/api/v1/zcode-plan/anthropic/v1/messages".into(),
            method: HttpMethod::Post,
            status: 200,
            value: json!({"type":"message","content":[]}),
        },
    ]);
    assert_eq!(
        plugin.fetch(request(stored, TRIAL)).await.unwrap().status,
        200
    );
    let sent = messages(&client);
    assert_eq!(sent.len(), 1);
    assert!(sent[0].0.starts_with("https://zcode.z.ai/"));
    assert_eq!(sent[0].1["model"], RAW);
}
