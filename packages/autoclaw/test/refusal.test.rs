use super::*;
use crate::tests::{collect, configured, credentials, envelope, Client, Sink};
use base64::{engine::general_purpose::STANDARD, Engine};

fn banned() -> Value {
    json!({"code":410004,"message":"The account has been banned"})
}

#[test]
fn account_refusals_only_use_the_documented_code_and_message_fields() {
    for value in [
        json!({"code":410004}),
        json!({"code":"410004","message":"Account unavailable"}),
        json!({"code":1,"msg":"账号已被封禁"}),
        json!({"error":{"message":"账号异常，请联系客服"}}),
        json!({"message":"The account has been suspended"}),
        json!({"message":"账户已禁用"}),
    ] {
        assert!(refusal::banned(&value).is_some(), "{value}");
    }
    for value in [
        json!({"code":401,"message":"token invalid"}),
        json!({"code":401001,"msg":"invalid_grant","detail":"The account has been banned"}),
        json!({"message":"unbanned"}),
        json!({"data":{"message":"The account has been banned"}}),
        json!("The account has been banned"),
    ] {
        assert!(refusal::banned(&value).is_none(), "{value}");
    }
}

#[tokio::test]
async fn a_successful_envelope_does_not_turn_its_message_into_an_account_refusal() {
    let client = Client::new(vec![(
        200,
        json!({"code":0,"message":"Account no longer banned","data":{"access_token":"new-access","refresh_token":"new-refresh"}}),
    )]);
    let provider = configured(Site::International, client);
    let result = provider
        .refresh(json!({"credentials":credentials(Site::International)}))
        .await
        .unwrap();
    assert_eq!(result["credentials"]["access"], "new-access");
}

fn request(site: Site, credentials: Value) -> FetchRequest {
    FetchRequest {
        subscription_accounts_enabled: true,
        account: "fixture".into(),
        credentials: Some(credentials),
        url: format!("{}/chat/completions", site.host()),
        method: "POST".into(),
        body: Some(STANDARD.encode(
            json!({"model":"zai_auto","messages":[{"role":"user","content":"hello"}]}).to_string(),
        )),
        headers: BTreeMap::new(),
        metadata: BTreeMap::new(),
        body_file: None,
        network_proxy: None,
        caller_user_agent: None,
        constraints: Default::default(),
    }
}

#[tokio::test]
async fn banned_chat_is_a_permission_refusal_without_signout_or_cross_host_retry() {
    for site in [Site::International, Site::China] {
        for status in [401, 403] {
            let client = Client::new(vec![(status, banned()), (status, banned())]);
            let mut provider = configured(site, client.clone());
            let sink = Arc::new(Sink::default());
            provider.sink = sink.clone();
            let saved = credentials(site);
            let result = provider.fetch(request(site, saved.clone())).await.unwrap();
            assert_eq!(result.status, 403);
            assert_eq!(result.headers["x-magpie-sign-in"], "kept");
            let body: Value = serde_json::from_slice(&collect(result).await).unwrap();
            assert_eq!(
                body["error"]["message"],
                "AutoClaw: The account has been banned"
            );
            assert_eq!(body["error"]["type"], "permission_error");
            assert_eq!(body["error"]["code"], 410004);
            assert_eq!(client.requests.lock().unwrap().len(), 1);
            assert!(sink.writes.lock().unwrap().is_empty());
            assert_eq!(
                provider
                    .accounts(CallContext {
                        credentials: Some(saved),
                        ..Default::default()
                    })
                    .await
                    .unwrap()["accounts"][0]["configured"],
                true
            );
        }
    }
}

#[tokio::test]
async fn banned_refresh_keeps_the_original_pair_and_never_rotates_or_tries_another_host() {
    for site in [Site::International, Site::China] {
        for (status, body, reason) in [
            (403, banned(), "The account has been banned"),
            (
                401,
                json!({"error":{"code":"410004","message":"账号已被封禁"}}),
                "账号已被封禁",
            ),
            (
                403,
                json!({"message":"The account is suspended"}),
                "The account is suspended",
            ),
            (200, json!({"code":410004}), "account refused (code 410004)"),
        ] {
            let client = Client::new(vec![(status, body)]);
            let mut provider = configured(site, client.clone());
            let sink = Arc::new(Sink::default());
            provider.sink = sink.clone();
            let saved = credentials(site);
            let error = provider
                .refresh(json!({"credentials":saved}))
                .await
                .unwrap_err();
            assert_eq!(error.code, "account_refused");
            assert_eq!(error.message, format!("AutoClaw: {reason}"));
            assert_eq!(client.requests.lock().unwrap().len(), 1);
            assert!(sink.writes.lock().unwrap().is_empty());
            assert_eq!(saved["access"], "access-one");
            assert_eq!(saved["refresh"], "refresh-one");
        }
    }
}

#[tokio::test]
async fn banned_usage_and_checkin_keep_the_reason_and_stop_before_other_hosts_or_claims() {
    for site in [Site::International, Site::China] {
        for method in ["usage", "checkin", "checkinStatus"] {
            let client = Client::new(vec![(403, banned()), (403, banned())]);
            let provider = configured(site, client.clone());
            let error = if method == "usage" {
                provider
                    .usage(CallContext {
                        credentials: Some(credentials(site)),
                        ..Default::default()
                    })
                    .await
                    .unwrap_err()
            } else {
                provider
                    .call(method, json!({"credentials":credentials(site)}))
                    .await
                    .unwrap_err()
            };
            assert_eq!(error.code, "account_refused", "{method}");
            assert_eq!(error.message, "AutoClaw: The account has been banned");
            let requests = client.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert!(requests.iter().all(|r| r.method == HttpMethod::Get));
        }
    }
}

#[tokio::test]
async fn a_ban_during_checkin_completion_is_not_login_expiry_or_a_second_claim() {
    let client = Client::new(vec![
        envelope(json!([{"task_id":"daily_signin","status":"pending"}])),
        (403, banned()),
    ]);
    let provider = configured(Site::China, client.clone());
    let error = provider
        .call("checkin", json!({"credentials":credentials(Site::China)}))
        .await
        .unwrap_err();
    assert_eq!(error.code, "account_refused");
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn ban_text_in_unrelated_fields_does_not_hide_actual_login_revocation() {
    for site in [Site::International, Site::China] {
        let body =
            json!({"code":401001,"message":"token expired","detail":"The account has been banned"});
        let client = Client::new(vec![(403, body.clone()), (403, body)]);
        let provider = configured(site, client);
        let result = provider
            .fetch(request(site, credentials(site)))
            .await
            .unwrap();
        assert_eq!(result.status, 401);
        assert_eq!(result.headers["x-magpie-sign-in"], "expired");
    }
}

#[tokio::test]
async fn revoked_refresh_is_permanent_but_network_and_wrong_host_failures_keep_login() {
    for (replies, expected) in [
        (
            vec![(401, json!({"raw":"invalid token"}))],
            "authentication_required",
        ),
        (
            vec![(403, json!({"code":401,"message":"token invalid"}))],
            "authentication_required",
        ),
        (
            vec![(200, json!({"code":401001,"msg":"invalid_grant"}))],
            "authentication_required",
        ),
        (
            vec![(400, json!({"code":1,"msg":"refresh token revoked"}))],
            "authentication_required",
        ),
        (
            vec![(503, json!({"code":1,"message":"temporarily unavailable"}))],
            "upstream_transport_error",
        ),
        (
            vec![(429, json!({"code":1,"message":"slow down"}))],
            "upstream_transport_error",
        ),
        (
            vec![(200, json!({"offline":true}))],
            "upstream_transport_error",
        ),
        (
            vec![(200, json!({"code":400000})), (200, json!({"code":400000}))],
            "upstream_transport_error",
        ),
    ] {
        let client = Client::new(replies);
        let mut provider = configured(Site::International, client);
        let sink = Arc::new(Sink::default());
        provider.sink = sink.clone();
        let error = provider
            .refresh(json!({"credentials":credentials(Site::International)}))
            .await
            .unwrap_err();
        assert_eq!(error.code, expected, "{error}");
        assert!(sink.writes.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn refreshing_before_chat_still_reports_a_ban_as_403_with_login_kept() {
    for site in [Site::International, Site::China] {
        let client = Client::new(vec![(403, banned())]);
        let provider = configured(site, client.clone());
        let mut saved = credentials(site);
        saved["expires"] = json!(0);
        let result = provider.fetch(request(site, saved)).await.unwrap();
        assert_eq!(result.status, 403);
        assert_eq!(result.headers["x-magpie-sign-in"], "kept");
        assert_eq!(client.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn account_refusal_reasons_redact_the_credentials_in_the_request() {
    for site in [Site::International, Site::China] {
        let client = Client::new(vec![(
            403,
            json!({"code":410004,"message":"Account banned: access-one refresh-one"}),
        )]);
        let provider = configured(site, client);
        let response = provider
            .fetch(request(site, credentials(site)))
            .await
            .unwrap();
        let body = String::from_utf8(collect(response).await).unwrap();
        assert!(body.contains("Account banned"));
        assert!(!body.contains("access-one"));
        assert!(!body.contains("refresh-one"));
        let client = Client::new(vec![(
            403,
            json!({"code":410004,"message":"Account banned: refresh-one"}),
        )]);
        let provider = configured(site, client);
        let error = provider
            .refresh(json!({"credentials":credentials(site)}))
            .await
            .unwrap_err();
        assert!(error.message.contains("Account banned"));
        assert!(!error.message.contains("refresh-one"));
    }
}

#[tokio::test]
async fn a_later_valid_login_can_refresh_after_an_account_refusal() {
    for site in [Site::International, Site::China] {
        let client = Client::new(vec![
            (403, banned()),
            envelope(
                json!({"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}),
            ),
        ]);
        let mut provider = configured(site, client.clone());
        let sink = Arc::new(Sink::default());
        provider.sink = sink.clone();
        let saved = credentials(site);
        assert_eq!(
            provider
                .refresh(json!({"credentials":saved}))
                .await
                .unwrap_err()
                .code,
            "account_refused"
        );
        let mut new_login = saved;
        new_login["refresh"] = json!("new-login-refresh");
        let result = provider
            .refresh(json!({"credentials":new_login}))
            .await
            .unwrap();
        assert_eq!(result["credentials"]["access"], "new-access");
        assert_eq!(result["credentials"]["refresh"], "new-refresh");
        assert_eq!(sink.writes.lock().unwrap().len(), 1);
        assert_eq!(client.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn an_account_refusal_on_a_legitimate_fallback_is_preserved() {
    for method in ["refresh-host", "refresh-agent", "fetch", "usage", "checkin"] {
        let first = match method {
            "refresh-host" => (200, json!({"code":400000})),
            "refresh-agent" => (200, json!({"code":400002})),
            "fetch" => (401, json!({"message":"token expired on wrong host"})),
            _ => (503, json!({"code":1})),
        };
        let client = Client::new(vec![first, (403, banned())]);
        let provider = configured(Site::International, client.clone());
        let saved = credentials(Site::International);
        if method == "fetch" {
            let response = provider
                .fetch(request(Site::International, saved))
                .await
                .unwrap();
            assert_eq!(response.status, 403);
            assert_eq!(response.headers["x-magpie-sign-in"], "kept");
        } else {
            let error = match method {
                "refresh-host" | "refresh-agent" => provider
                    .refresh(json!({"credentials":saved}))
                    .await
                    .unwrap_err(),
                "usage" => provider
                    .usage(CallContext {
                        credentials: Some(saved),
                        ..Default::default()
                    })
                    .await
                    .unwrap_err(),
                _ => provider
                    .call("checkin", json!({"credentials":saved}))
                    .await
                    .unwrap_err(),
            };
            assert_eq!(error.code, "account_refused");
            assert_eq!(error.message, "AutoClaw: The account has been banned");
        }
        assert_eq!(client.requests.lock().unwrap().len(), 2, "{method}");
    }
}
