use super::*;

#[path = "team_gift_protocol.test.rs"]
mod protocol;

const RAW: &str = "glm-5.3-flash";
const TRIAL: &str = "GLM-5.3-Flash-Trial";
const BALANCE: &str = "/api/v1/zcode-plan/billing/balance?app_version=3.14.3";

fn stored(selected: Site, key: &str, token: &str) -> Value {
    let mut value = credentials(key, token);
    value["authMethod"] = json!(format!("{}-team", selected.id()));
    value["state"] = json!({"site":selected.id(),"base":selected.base(),"key":key,"jwt":token,"token":"team-token","org":"team-org","project":"team-project","device":"team-device","plan":"Team Max"});
    value
}

fn context(stored: &Value) -> CallContext {
    CallContext {
        account: "team-account".into(),
        credentials: Some(stored.clone()),
        subscription_accounts_enabled: true,
        ..Default::default()
    }
}

fn balance(remaining: u64) -> Value {
    json!({"server_time":1000,"plans":[{"plan_id":"dev-day","name":"Dev Day","status":"active","ends_at":4600}],"balances":[{"plan_id":"dev-day","show_name":"GLM-5.3-Flash","capabilities":[format!("model:{RAW}")],"total_units":100,"used_units":100-remaining,"remaining_units":remaining,"expires_at":4600}]})
}

fn team_usage(percent: u64) -> Vec<Reply> {
    vec![
        reply(
            "/api/monitor/usage/quota/limit?type=2",
            json!({"limits":[{"type":"CREDIT_LIMIT","unit":6,"number":1,"percentage":percent}]}),
        ),
        reply(
            "/api/biz/team/subscribe/product/querySubscribeDetail",
            json!({"hasSubscription":true,"status":"EFFECTIVE","memberGrantStatus":"VALID","productName":"Team Max","subscribeEndTime":1798888888}),
        ),
        reply(
            "/api/biz/customer-package-reset/list?targetType=TEAM",
            json!({"fiveHourResets":[{"available":true,"expireTime":1798888888}]}),
        ),
    ]
}

fn preview() -> Reply {
    reply(
        format!(
            "/api/v1/zcode-plan/billing/preview?app_version={}&platform={}",
            endpoint::VERSION,
            endpoint::platform()
        ),
        json!({"plans":[{"plan_id":"trust","name":"Trust Build"}]}),
    )
}

fn message(start: bool, status: u16, value: Value) -> Reply {
    Reply {
        path: if start {
            "/api/v1/zcode-plan/anthropic/v1/messages"
        } else {
            "/api/anthropic/v1/messages"
        }
        .into(),
        method: HttpMethod::Post,
        status,
        value,
    }
}

async fn configured(replies: Vec<Reply>) -> (Zcode, Arc<Client>, Arc<Store>) {
    let (plugin, client, store) = setup(replies);
    *plugin.config.lock().await = Some((core::now(), json!({})));
    (plugin, client, store)
}

fn has_trial(listing: &Value) -> bool {
    listing["models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|model| model["id"] == TRIAL)
}

fn assert_personal(request: &HttpRequest, jwt: &str) {
    assert!(request.url.starts_with("https://zcode.z.ai/"));
    assert_eq!(request.headers["authorization"], format!("Bearer {jwt}"));
    for key in ["x-api-key", "bigmodel-organization", "bigmodel-project"] {
        assert!(!request.headers.contains_key(key));
    }
}

#[tokio::test]
async fn models_list_personal_trials_without_resolving_a_team_key_or_reading_personal_subscriptions(
) {
    for selected in [Site::Zai, Site::BigModel] {
        for key in ["team.secret", ""] {
            let token = jwt();
            let stored = stored(selected, key, &token);
            let (plugin, client, store) = configured(vec![reply(BALANCE, balance(75))]).await;
            let models = plugin.models(context(&stored)).await.unwrap();
            assert!(has_trial(&models));
            assert!(models["models"]
                .as_array()
                .unwrap()
                .iter()
                .any(|model| model["id"] == "GLM-5.3-Flash"));
            assert!(!plugin.route(client.as_ref(), &stored["state"]).await.start);
            let requests = client.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_personal(&requests[0], &token);
            assert!(store.saved.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn team_and_personal_gift_windows_remain_independent_even_when_one_pool_is_spent() {
    for selected in [Site::Zai, Site::BigModel] {
        for (team_percent, gift_left) in [(100, 75), (25, 0), (25, 75), (100, 0)] {
            let token = jwt();
            let stored = stored(selected, "team.secret", &token);
            let mut replies = vec![
                reply(BALANCE, balance(gift_left)),
                reply(BALANCE, balance(gift_left)),
                preview(),
            ];
            replies.extend(team_usage(team_percent));
            let (plugin, client, store) = configured(replies).await;
            let card = plugin.usage(context(&stored)).await.unwrap();
            assert_eq!(card["available"], true);
            assert_eq!(card["plan"], "Team Max");
            assert_eq!(
                card["items"][0]["usedPercent"].as_f64(),
                Some(team_percent as f64)
            );
            assert_eq!(card["items"][0]["notModels"], json!([TRIAL]));
            assert_eq!(card["items"][1]["models"], json!([TRIAL]));
            assert_eq!(
                card["items"][1]["usedPercent"].as_f64(),
                Some((100 - gift_left) as f64)
            );
            assert_eq!(card["resets"]["team"], true);
            assert_eq!(card["resets"]["count"], 1);
            assert!(card["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(
                    |item| item["label"] == "Gift plans to claim in the ZCode app"
                        && item["countsForRouting"] == false
                ));
            let requests = client.requests.lock().unwrap();
            for request in requests.iter() {
                if request.url.contains("/billing/") {
                    assert_personal(request, &token);
                } else {
                    assert!(request.url.starts_with(selected.api()));
                    assert_eq!(request.headers["bigmodel-organization"], "team-org");
                    assert_eq!(request.headers["bigmodel-project"], "team-project");
                    assert_eq!(
                        request.headers["authorization"],
                        if request.url.contains("/usage/") {
                            "team.secret"
                        } else {
                            "team-token"
                        }
                    );
                }
            }
            assert!(client.replies.lock().unwrap().is_empty());
            assert!(store.saved.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn ordinary_models_use_the_team_key_while_trials_use_only_the_personal_jwt() {
    for selected in [Site::Zai, Site::BigModel] {
        let token = jwt();
        let stored = stored(selected, "team.secret", &token);
        let (plugin, client, store) = configured(vec![
            reply(BALANCE, balance(75)),
            message(false, 200, json!({"type":"message","content":[]})),
            message(true, 200, json!({"type":"message","content":[]})),
        ])
        .await;
        assert!(has_trial(&plugin.models(context(&stored)).await.unwrap()));
        for model in ["GLM-5.3-Flash", TRIAL] {
            let mut request = request(stored.clone(), model);
            request
                .headers
                .insert("bigmodel-organization".into(), "untrusted-org".into());
            request
                .headers
                .insert("bigmodel-project".into(), "untrusted-project".into());
            request
                .headers
                .insert("x-api-key".into(), "untrusted-key".into());
            assert_eq!(plugin.fetch(request).await.unwrap().status, 200);
        }
        let requests = client.requests.lock().unwrap();
        let messages: Vec<_> = requests
            .iter()
            .filter(|request| request.url.ends_with("/messages"))
            .collect();
        assert_eq!(messages.len(), 2);
        assert!(messages[0].url.starts_with(selected.base()));
        assert_eq!(messages[0].headers["x-api-key"], "team.secret");
        assert_eq!(messages[0].headers["authorization"], "Bearer team.secret");
        assert_personal(messages[1], &token);
        let body: Value = serde_json::from_slice(messages[1].body.as_ref().unwrap()).unwrap();
        assert_eq!(body["model"], RAW);
        assert_eq!(body["tools"][0]["name"], "read");
        assert_eq!(body["top_p"], 0.5);
        assert!(store.saved.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn failed_gift_reads_keep_the_team_card_and_retry_within_a_minute() {
    for status in [0, 200, 401, 403, 429, 503] {
        let stored = stored(Site::BigModel, "team.secret", &jwt());
        let failed = || Reply {
            path: BALANCE.into(),
            method: HttpMethod::Get,
            status,
            value: Value::Null,
        };
        let mut replies = vec![failed(), failed(), preview()];
        replies.extend(team_usage(90));
        let (plugin, client, store) = configured(replies).await;
        plugin.models(context(&stored)).await.unwrap();
        let card = plugin.usage(context(&stored)).await.unwrap();
        assert_eq!(card["available"], true);
        assert_eq!(card["items"][0]["usedPercent"], 90.0);
        let route = plugin.route(client.as_ref(), &stored["state"]).await;
        assert!(!route.start);
        assert!(route.gift.is_none());
        assert!(route.ttl <= 60_000);
        assert!(store.saved.lock().unwrap().is_empty());
        assert!(client.replies.lock().unwrap().is_empty());
        plugin
            .routes
            .lock()
            .await
            .get_mut(&route_key(&stored["state"]))
            .unwrap()
            .at = core::now().saturating_sub(60_001);
        client
            .replies
            .lock()
            .unwrap()
            .push_back(reply(BALANCE, balance(75)));
        assert!(has_trial(&plugin.models(context(&stored)).await.unwrap()));
    }
}

#[tokio::test]
async fn absent_or_expired_zcode_sessions_explain_missing_trials_without_billing_calls() {
    let expired = format!(
        "e30.{}.signature",
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&json!({"exp":1})).unwrap())
    );
    for token in ["", expired.as_str()] {
        let stored = stored(Site::BigModel, "team.secret", token);
        let (plugin, client, store) = configured(team_usage(90)).await;
        assert!(!has_trial(&plugin.models(context(&stored)).await.unwrap()));
        let card = plugin.usage(context(&stored)).await.unwrap();
        assert_eq!(card["available"], true);
        let hint = card["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["label"] == "ZCode gift plans (-Trial models) need ZCode's sign-in")
            .unwrap();
        assert_eq!(hint["countsForRouting"], false);
        assert!(hint["display"]
            .as_str()
            .unwrap()
            .contains(if token.is_empty() {
                "no ZCode session"
            } else {
                "expired"
            }));
        assert!(client
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| !request.url.contains("/billing/")));
        assert!(store.saved.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn fresh_usage_replenishes_personal_trials_without_relaxing_exhausted_team_windows() {
    let stored = stored(Site::Zai, "team.secret", &jwt());
    let (plugin, client, _) = configured(vec![reply(BALANCE, balance(0))]).await;
    assert!(has_trial(&plugin.models(context(&stored)).await.unwrap()));
    assert_eq!(
        plugin
            .fetch(request(stored.clone(), TRIAL))
            .await
            .unwrap()
            .status,
        429
    );
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    client.replies.lock().unwrap().extend(team_usage(100));
    client.replies.lock().unwrap().extend([
        reply(BALANCE, balance(75)),
        preview(),
        message(true, 200, json!({"type":"message","content":[]})),
    ]);
    let card = plugin.usage(context(&stored)).await.unwrap();
    assert_eq!(card["items"][0]["usedPercent"], 100.0);
    assert_eq!(card["items"][0]["notModels"], json!([TRIAL]));
    assert_eq!(
        plugin.fetch(request(stored, TRIAL)).await.unwrap().status,
        200
    );
    assert!(client.replies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn gift_refusals_keep_the_team_and_never_retry_against_its_paid_plan() {
    for (status, error, expected, blocked) in [
        (
            200,
            json!({"code":1005,"msg":"exceed quota limit"}),
            429,
            true,
        ),
        (429, json!({"error":"limited"}), 429, true),
        (403, json!({"error":"denied"}), 403, false),
        (
            405,
            json!({"code":3012,"msg":"unusual activity"}),
            405,
            false,
        ),
        (503, json!({"error":"unavailable"}), 503, false),
    ] {
        let stored = stored(Site::BigModel, "team.secret", &jwt());
        let (plugin, client, store) = configured(vec![
            reply(BALANCE, balance(75)),
            message(true, status, error.clone()),
            message(false, 200, json!({"type":"message","content":[]})),
        ])
        .await;
        assert!(has_trial(&plugin.models(context(&stored)).await.unwrap()));
        let response = plugin.fetch(request(stored.clone(), TRIAL)).await.unwrap();
        assert_eq!(response.status, expected);
        let response = decode(response).await;
        if status == 405 {
            assert_eq!(response["error"]["type"], "permission_error");
            assert!(response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("IP block"));
        } else {
            assert_eq!(response, error);
        }
        assert_eq!(
            client
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|request| request.url.ends_with("/messages"))
                .count(),
            1
        );
        if blocked {
            assert_eq!(
                plugin
                    .fetch(request(stored.clone(), TRIAL))
                    .await
                    .unwrap()
                    .status,
                429
            );
        }
        assert_eq!(
            plugin
                .fetch(request(stored, "GLM-5.3-Flash"))
                .await
                .unwrap()
                .status,
            200
        );
        assert_eq!(
            client
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|request| request.url.ends_with("/messages"))
                .count(),
            2
        );
        assert!(store.saved.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn unresolved_team_keys_are_saved_only_when_an_account_operation_needs_them() {
    let token = jwt();
    let stored = stored(Site::BigModel, "", &token);
    let (plugin, client, store) = configured(vec![reply(BALANCE, balance(75))]).await;
    assert!(has_trial(&plugin.models(context(&stored)).await.unwrap()));
    assert!(store.saved.lock().unwrap().is_empty());
    client.replies.lock().unwrap().extend([
        reply(
            "/api_keys",
            json!([{"name":"zcode-team-api-key","keyType":2,"apiKey":"team-id"}]),
        ),
        reply("/api_keys/copy/team-id", json!({"secretKey":"secret"})),
        reply(BALANCE, balance(75)),
        message(true, 200, json!({"type":"message","content":[]})),
        message(false, 200, json!({"type":"message","content":[]})),
    ]);
    assert_eq!(
        plugin
            .fetch(request(stored.clone(), TRIAL))
            .await
            .unwrap()
            .status,
        200
    );
    assert_eq!(
        plugin
            .fetch(request(stored, "GLM-5.3-Flash"))
            .await
            .unwrap()
            .status,
        200
    );
    let saved = store.saved.lock().unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0]["uid"], "user-one");
    assert_eq!(saved[0]["state"]["key"], "team-id.secret");
    assert_eq!(saved[0]["state"]["jwt"], token);
    let requests = client.requests.lock().unwrap();
    let keys: Vec<_> = requests
        .iter()
        .filter(|request| request.url.contains("/api_keys"))
        .collect();
    assert_eq!(keys.len(), 2);
    for request in keys {
        assert_eq!(request.method, HttpMethod::Get);
        assert_eq!(request.headers["authorization"], "team-token");
        assert_eq!(request.headers["bigmodel-organization"], "team-org");
        assert_eq!(request.headers["bigmodel-project"], "team-project");
    }
    assert_personal(
        requests
            .iter()
            .find(|request| request.url.starts_with(endpoint::START))
            .unwrap(),
        &token,
    );
    assert_eq!(
        requests.last().unwrap().headers["x-api-key"],
        "team-id.secret"
    );
    assert!(client.replies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn discovering_gifts_never_bypasses_denied_team_key_access_or_creates_keys() {
    for status in [200, 403] {
        let stored = stored(Site::BigModel, "", &jwt());
        let (plugin, client, store) = configured(vec![reply(BALANCE, balance(75))]).await;
        assert!(has_trial(&plugin.models(context(&stored)).await.unwrap()));
        let mut denied = reply("/api_keys", json!([]));
        denied.status = status;
        client.replies.lock().unwrap().push_back(denied);
        assert!(plugin.fetch(request(stored, TRIAL)).await.is_err());
        assert_eq!(client.requests.lock().unwrap().len(), 2);
        assert!(client
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.method == HttpMethod::Get));
        assert!(store.saved.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn team_quota_permission_errors_are_not_replaced_with_a_successful_gift_card() {
    let stored = stored(Site::BigModel, "team.secret", &jwt());
    let (plugin, client, store) = configured(vec![reply(BALANCE, balance(75))]).await;
    assert!(has_trial(&plugin.models(context(&stored)).await.unwrap()));
    let mut denied = reply("/api/monitor/usage/quota/limit?type=2", json!({}));
    denied.status = 403;
    client.replies.lock().unwrap().push_back(denied);
    let usage = plugin.usage(context(&stored)).await.unwrap();
    assert_eq!(usage["available"], false);
    assert_eq!(usage["plan"], "Team Max");
    assert!(usage["reason"].as_str().unwrap().contains("403"));
    assert_eq!(client.requests.lock().unwrap().len(), 2);
    assert!(store.saved.lock().unwrap().is_empty());
}

#[tokio::test]
async fn personal_trial_allowances_and_blocks_do_not_cross_accounts_in_one_plugin() {
    let token_a = jwt();
    let token_b = format!(
        "e30.{}.signature",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({"exp":core::now()/1000+3600,"sub":"user-two"})).unwrap()
        )
    );
    let a = stored(Site::BigModel, "team-a.secret", &token_a);
    let mut b = stored(Site::Zai, "team-b.secret", &token_b);
    b["uid"] = json!("user-two");
    let (plugin, client, store) = configured(vec![
        reply(BALANCE, balance(75)),
        reply(BALANCE, balance(75)),
        message(true, 429, json!({"error":"limited"})),
        message(true, 200, json!({"type":"message","content":[]})),
    ])
    .await;
    for stored in [&a, &b] {
        assert!(has_trial(&plugin.models(context(stored)).await.unwrap()));
    }
    assert_eq!(plugin.fetch(request(a, TRIAL)).await.unwrap().status, 429);
    assert_eq!(plugin.fetch(request(b, TRIAL)).await.unwrap().status, 200);
    let requests = client.requests.lock().unwrap();
    let messages: Vec<_> = requests
        .iter()
        .filter(|request| request.url.ends_with("/messages"))
        .collect();
    assert_eq!(messages.len(), 2);
    assert_personal(messages[0], &token_a);
    assert_personal(messages[1], &token_b);
    assert!(store.saved.lock().unwrap().is_empty());
}
