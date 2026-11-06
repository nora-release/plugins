use super::*;
use crate::openai_provider::usage::resets;
use async_trait::async_trait;
use futures::stream;
use nora_ai::{HttpClientError, HttpMethod, HttpStreamingResponse};
use serde_json::json;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

async fn fetch_usage(
    plugin: &str,
    auth: &ResolvedProviderAuth,
    account_id: Option<&str>,
    client: &dyn HttpClient,
) -> Result<Value, String> {
    match plugin {
        "openai" => crate::openai_provider::usage::fetch(auth, account_id, client).await,
        "xai" => crate::xai_provider::usage::fetch(auth, account_id, client).await,
        "anthropic" => crate::anthropic_provider::usage::fetch(auth, account_id, client).await,
        "github-copilot" => crate::copilot_provider::usage::fetch(auth, account_id, client).await,
        _ => Ok(unavailable("This provider does not expose a supported subscription usage endpoint for this account.")),
    }
}

struct Reply {
    status: u16,
    body: Vec<u8>,
}
impl Reply {
    fn json(value: Value) -> Self {
        Self {
            status: 200,
            body: serde_json::to_vec(&value).unwrap(),
        }
    }
    fn status(status: u16) -> Self {
        Self {
            status,
            body: br#"{"error":"private-token"}"#.to_vec(),
        }
    }
}
struct MockClient {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<Reply>>,
}
#[async_trait]
impl HttpClient for MockClient {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected upstream request");
        Ok(HttpStreamingResponse {
            status: reply.status,
            headers: BTreeMap::new(),
            body: Box::pin(stream::iter(vec![Ok(reply.body.into())])),
        })
    }
}
fn client(replies: Vec<Reply>) -> MockClient {
    MockClient {
        requests: Mutex::new(Vec::new()),
        replies: Mutex::new(replies.into()),
    }
}
fn auth(provider: &str) -> ResolvedProviderAuth {
    ResolvedProviderAuth {
        token: Some("private-token".into()),
        credential_provider_id: Some(provider.into()),
        ..Default::default()
    }
}
fn item<'a>(result: &'a Value, id: &str) -> &'a Value {
    result["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == id)
        .unwrap()
}
fn balance<'a>(result: &'a Value, id: &str) -> &'a Value {
    result["balances"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == id)
        .unwrap()
}

#[tokio::test]
async fn codex_usage_reads_credits_and_reset_details_with_the_selected_account() {
    let client = client(vec![
        Reply::json(
            json!({"email":"work@example.com","plan_type":"pro","rate_limit":{"primary_window":{"used_percent":42,"limit_window_seconds":18000,"reset_at":1791640059}},"credits":{"has_credits":true,"unlimited":false,"balance":"28263.2358130000"},"rate_limit_reset_credits":{"available_count":2}}),
        ),
        Reply::json(
            json!({"available_count":2,"credits":[{"id":"a","status":"available","granted_at":"2026-09-01T00:00:00Z","expires_at":"2026-11-01T00:00:00Z"},{"id":"b","status":"redeemed","redeemed_at":"2026-09-02T00:00:00Z"}]}),
        ),
    ]);
    let result = fetch_usage(
        "openai",
        &auth("openai-codex"),
        Some("account-123"),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(
        result["account"],
        json!({"email":"work@example.com","plan":"pro"})
    );
    assert_eq!(
        item(&result, "rate_limit:primary_window")["usedPercent"],
        42.0
    );
    assert_eq!(balance(&result, "credits")["amount"], 28263.235813);
    assert_eq!(result["resetCredits"]["totalAvailable"], 2);
    assert_eq!(result["resetCredits"]["grants"][0]["status"], "available");
    assert_eq!(result["resetCredits"]["grants"][1]["status"], "redeemed");
    assert!(parsing::timestamp(&result["updatedAt"]).is_some());
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].url.ends_with("/wham/usage"));
    assert!(requests[1].url.ends_with("/wham/rate-limit-reset-credits"));
    for request in requests.iter() {
        assert_eq!(request.method, HttpMethod::Get);
        assert_eq!(request.headers["chatgpt-account-id"], "account-123");
        assert_eq!(request.headers["authorization"], "Bearer private-token");
        assert!(request.body.is_none());
        assert!(!request.url.contains("consume"));
    }
    assert!(!result.to_string().contains("private-token"));
}

#[tokio::test]
async fn codex_reset_failure_preserves_usage_balance_and_the_known_count() {
    for reply in [
        Reply::status(500),
        Reply {
            status: 200,
            body: b"not-json".to_vec(),
        },
        Reply::json(json!({})),
    ] {
        let client = client(vec![
            Reply::json(
                json!({"rate_limit":{"primary_window":{"used_percent":0}},"credits":{"balance":"0","unlimited":false},"rate_limit_reset_credits":{"available_count":0}}),
            ),
            reply,
        ]);
        let result = fetch_usage("openai", &auth("openai-codex"), Some("account"), &client)
            .await
            .unwrap();
        assert_eq!(result["available"], true);
        assert_eq!(
            item(&result, "rate_limit:primary_window")["usedPercent"],
            0.0
        );
        assert_eq!(balance(&result, "credits")["amount"], 0.0);
        assert_eq!(result["resetCredits"]["available"], true);
        assert_eq!(result["resetCredits"]["totalAvailable"], 0);
        assert!(result["resetCredits"]["message"].is_string());
        assert!(!result.to_string().contains("private-token"));
    }
}

#[tokio::test]
async fn missing_reset_totals_stay_unknown_after_failed_details() {
    let client = client(vec![
        Reply::json(json!({"rate_limit":{"primary_window":{"used_percent":64}}})),
        Reply::status(403),
    ]);
    let result = fetch_usage("openai", &auth("openai-codex"), Some("account"), &client)
        .await
        .unwrap();
    assert_eq!(result["available"], true);
    assert_eq!(result["resetCredits"]["available"], false);
    assert!(result["resetCredits"].get("totalAvailable").is_none());
}

#[tokio::test]
async fn usage_reset_count_survives_an_empty_or_truncated_detail_list() {
    for details in [
        json!({"credits":[]}),
        json!({"credits":[{"id":"listed","status":"available"}]}),
    ] {
        let client = client(vec![
            Reply::json(json!({"rate_limit_reset_credits":{"available_count":5}})),
            Reply::json(details),
        ]);
        let result = fetch_usage("openai", &auth("openai-codex"), Some("account"), &client)
            .await
            .unwrap();
        assert_eq!(result["resetCredits"]["available"], true);
        assert_eq!(result["resetCredits"]["totalAvailable"], 5);
    }
}

#[test]
fn codex_additional_windows_spend_caps_and_unlimited_balance_are_preserved() {
    let result = parse_usage(
        "openai",
        &json!({
            "rate_limit":{"primary_window":{"used_percent":100,"limit_window_seconds":604800}},
            "code_review_rate_limit":{"primary_window":{"used_percent":5,"limit_window_seconds":604800}},
            "additional_rate_limits":[{"limit_name":"Fast model","metered_feature":"fast","rate_limit":{"primary_window":{"used_percent":19,"limit_window_seconds":18000},"secondary_window":{"used_percent":20}}}],
            "credits":{"unlimited":true,"balance":null},
            "spend_control":{"individual_limit":{"limit":"0","used":"0.0","remaining":"0.0","used_percent":100,"reset_at":1793491201}}
        }),
    );
    assert_eq!(
        item(&result, "rate_limit:primary_window")["label"],
        "Weekly"
    );
    assert_eq!(
        item(&result, "code_review:primary_window")["usedPercent"],
        5.0
    );
    assert_eq!(
        item(&result, "additional:fast:primary_window")["usedPercent"],
        19.0
    );
    assert_eq!(
        item(&result, "additional:fast:secondary_window")["usedPercent"],
        20.0
    );
    assert_eq!(item(&result, "spend_control")["remaining"], 0.0);
    assert_eq!(balance(&result, "credits")["unlimited"], true);
    assert!(balance(&result, "credits").get("amount").is_none());
}

#[test]
fn codex_unknown_balances_are_not_replaced_with_zero() {
    let unknown = parse_usage(
        "openai",
        &json!({"credits":{"has_credits":true,"unlimited":false,"balance":null}}),
    );
    assert!(balance(&unknown, "credits").get("amount").is_none());
    assert!(unknown["resetCredits"].get("totalAvailable").is_none());
    let empty = parse_usage("openai", &json!({}));
    assert_eq!(empty["available"], false);
    assert!(empty["balances"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn grok_usage_reuses_the_billing_contract_and_preserves_credit_amounts() {
    let client = client(vec![Reply::json(
        json!({"subscriptionTier":"supergrok","config":{"creditUsagePercent":25,"used":{"val":"250"},"monthlyLimit":{"val":"1000"},"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","end":"2026-11-01T00:00:00Z"},"onDemandUsed":{"val":500},"onDemandCap":{"val":2000},"prepaidBalance":{"val":0}}}),
    )]);
    let mut auth = auth("xai");
    auth.headers
        .insert("x-grok-client-version".into(), "1.2.3".into());
    let result = fetch_usage("xai", &auth, Some("grok-user"), &client)
        .await
        .unwrap();
    assert_eq!(result["account"]["plan"], "supergrok");
    assert_eq!(item(&result, "included_credits")["usedPercent"], 25.0);
    assert_eq!(item(&result, "included_credits")["used"], 250.0);
    assert_eq!(item(&result, "included_credits")["limit"], 1000.0);
    assert_eq!(balance(&result, "included_credits")["amount"], 750.0);
    assert_eq!(balance(&result, "on_demand")["amount"], 1500.0);
    assert_eq!(balance(&result, "prepaid_credits")["amount"], 0.0);
    assert!(result["resetCredits"].is_null());
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].headers["x-userid"], "grok-user");
    assert_eq!(requests[0].headers["x-grok-client-version"], "1.2.3");
    assert!(!requests[0].headers.contains_key("x-authenticateresponse"));
}

#[test]
fn anthropic_keeps_fractional_percentages_scoped_windows_and_extra_credit_totals() {
    let result = parse_usage(
        "anthropic",
        &json!({
            "five_hour":{"utilization":0.25},"seven_day":{"used_percentage":62},
            "seven_day_opus":{"utilization":8,"resets_at":"2026-11-01T00:00:00Z"},
            "extra_usage":{"is_enabled":true,"monthly_limit":1000,"used_credits":250,"utilization":25}
        }),
    );
    assert_eq!(item(&result, "five_hour")["usedPercent"], 0.25);
    assert_eq!(item(&result, "seven_day")["usedPercent"], 62.0);
    assert_eq!(item(&result, "seven_day_opus")["usedPercent"], 8.0);
    assert_eq!(balance(&result, "extra_usage")["amount"], 750.0);
}

#[test]
fn copilot_preserves_remaining_counts_and_separates_unlimited_from_exhausted_pools() {
    let result = parse_usage(
        "github-copilot",
        &json!({"copilot_plan":"business","quota_reset_date_utc":"2026-11-01T00:00:00Z","quota_snapshots":{
            "premium_interactions":{"entitlement":"100","quota_remaining":"70","quota_reset_at":1793491201},
            "chat":{"unlimited":true,"has_quota":false},"completions":{"entitlement":500,"remaining":0}
        }}),
    );
    assert_eq!(item(&result, "premium_interactions")["usedPercent"], 30.0);
    assert_eq!(item(&result, "premium_interactions")["remaining"], 70.0);
    assert_eq!(item(&result, "completions")["usedPercent"], 100.0);
    assert_eq!(balance(&result, "chat")["unlimited"], true);
    assert_eq!(result["account"]["plan"], "Business");
    let exhausted = parse_usage(
        "github-copilot",
        &json!({"quota_snapshots":{"premium_interactions":{"unlimited":true,"has_quota":false}}}),
    );
    assert_eq!(
        item(&exhausted, "premium_interactions")["usedPercent"],
        100.0
    );
    assert!(exhausted["balances"].as_array().unwrap().is_empty());
}

#[test]
fn copilot_real_token_billing_counts_override_rounded_percentages() {
    let result = parse_usage(
        "github-copilot",
        &json!({
            "copilot_plan":"individual_max",
            "quota_reset_date_utc":"2026-11-01T00:00:00.000Z",
            "quota_snapshots":{"premium_interactions":{
                "entitlement":20000,"quota_remaining":19999.9,"remaining":19999,
                "percent_remaining":99.9,"unlimited":false,"has_quota":true,
                "quota_reset_at":0,"token_based_billing":true
            }}
        }),
    );
    let premium = item(&result, "premium_interactions");
    assert!((premium["used"].as_f64().unwrap() - 0.1).abs() < 1e-8);
    assert!((premium["usedPercent"].as_f64().unwrap() - 0.0005).abs() < 1e-10);
    assert_eq!(premium["remaining"], 19999.9);
    assert_eq!(premium["value"], "0.1 / 20000 requests");
}

#[test]
fn copilot_reset_date_falls_back_when_utc_date_is_missing_or_invalid() {
    for utc in [Value::Null, json!("not-a-date")] {
        let result = parse_usage(
            "github-copilot",
            &json!({
                "quota_reset_date_utc":utc,"quota_reset_date":"2026-11-01",
                "quota_snapshots":{"premium_interactions":{"entitlement":100,"quota_remaining":70,"quota_reset_at":0}}
            }),
        );
        assert_eq!(
            item(&result, "premium_interactions")["resetsAt"],
            "2026-11-01T00:00:00+00:00"
        );
    }
}

#[tokio::test]
async fn copilot_usage_follows_the_verified_accounts_enterprise_host() {
    use crate::ProviderAdapter;
    for (host, expected) in [
        (None, "https://api.github.com/copilot_internal/user"),
        (Some(""), "https://api.github.com/copilot_internal/user"),
        (
            Some("https://Acme.ghe.com/"),
            "https://api.acme.ghe.com/copilot_internal/user",
        ),
    ] {
        let mut extras = BTreeMap::new();
        if let Some(host) = host {
            extras.insert("githubHost".into(), nora_ai::JsonValue::string(host));
        }
        let credentials = nora_ai::OAuthCredentials::new(
            nora_ai::OAuthProviderId::GitHubCopilot,
            "chat-session-token",
            Some("verified-github-token".into()),
            None,
            extras,
        );
        let mut auth = crate::auth::base_auth("github-copilot", &credentials);
        crate::copilot_provider::Copilot::default().enrich_auth(&credentials, &mut auth);
        assert_eq!(auth.headers["editor-version"], "vscode/1.140.0");
        assert_eq!(auth.headers["editor-plugin-version"], "copilot-chat/0.68.0");
        assert_eq!(auth.headers["x-github-api-version"], "2026-01-09");
        let client = client(vec![Reply::json(json!({"quota_snapshots":{
            "chat":{"entitlement":200,"quota_remaining":150}
        }}))]);
        let result = fetch_usage("github-copilot", &auth, None, &client)
            .await
            .unwrap();
        assert_eq!(item(&result, "chat")["usedPercent"], 25.0);
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url, expected);
        assert_eq!(
            requests[0].headers["authorization"],
            "token verified-github-token"
        );
        assert!(requests[0].body.is_none());
        assert_eq!(requests[0].headers["editor-version"], "vscode/1.140.0");
        assert_eq!(
            requests[0].headers["editor-plugin-version"],
            "copilot-chat/0.68.0"
        );
        assert_eq!(
            requests[0].headers["user-agent"],
            "GitHubCopilotChat/0.68.0"
        );
    }
}

#[tokio::test]
async fn copilot_usage_rejects_unsafe_saved_hosts_before_sending_the_account_token() {
    for host in [
        json!("github.com@evil.invalid"),
        json!("api.acme.ghe.com"),
        json!("https://acme.ghe.com/path"),
        json!(false),
    ] {
        let mut auth = auth("github-copilot");
        auth.metadata.insert(
            "githubOAuthToken".into(),
            nora_ai::JsonValue::string("private-token"),
        );
        auth.metadata.insert("githubHost".into(), host.into());
        let client = client(Vec::new());
        let error = fetch_usage("github-copilot", &auth, None, &client)
            .await
            .unwrap_err();
        assert_eq!(error, "The GitHub account host is invalid.");
        assert!(!error.contains("private-token"));
        assert!(client.requests.lock().unwrap().is_empty());
    }
}

#[test]
fn reset_credit_counts_are_authoritative_even_when_the_detail_list_is_truncated() {
    let value = resets::parse(
        &json!({"available_count":5,"credits":[{"id":"a","status":"available","granted_at":"2026-09-01T00:00:00Z","expires_at":null}]}),
    );
    assert_eq!(value["totalAvailable"], 5);
    assert_eq!(value["grants"].as_array().unwrap().len(), 1);
    assert!(value["grants"][0].get("expiresAt").is_none());
}

#[test]
fn reset_credits_accept_the_skill_scripts_field_aliases_without_assuming_availability() {
    let value = resets::parse(
        &json!({"data":[{"creditId":"redeemed","state":"redeemed","issuedAt":1790812800000_i64,"redeemedAt":"2026-10-02T00:00:00Z","profile_user_id":"private-user","access_token":"private-token"},{"credit_id":"pending","grantedAt":"2026-09-01T00:00:00Z","redeemStartedAt":"2026-10-01T00:00:00Z"},{"id":"unknown","is_redeemed":false}]}),
    );
    assert_eq!(value["available"], true);
    assert!(value.get("totalAvailable").is_none());
    assert_eq!(value["grants"][0]["status"], "redeemed");
    assert_eq!(value["grants"][1]["status"], "redeeming");
    assert_eq!(value["grants"][2]["status"], "not_redeemed");
    assert!(!value.to_string().contains("private-user"));
    assert!(!value.to_string().contains("private-token"));
}

#[test]
fn reset_unknown_and_true_zero_have_different_results() {
    for value in [
        json!({}),
        json!({"credits":null}),
        json!({"available_count":null}),
        json!({"available_count":-1}),
        json!({"available_count":"NaN"}),
    ] {
        let result = resets::parse(&value);
        assert_eq!(result["available"], false);
        assert!(result.get("totalAvailable").is_none());
    }
    for value in [
        json!({"available_count":0}),
        json!({"credits":[]}),
        json!([]),
    ] {
        let result = resets::parse(&value);
        assert_eq!(result["available"], true);
        assert_eq!(result["totalAvailable"], 0);
    }
}

#[tokio::test]
async fn api_key_accounts_and_copilot_chat_tokens_do_not_call_oauth_endpoints() {
    let client = client(vec![]);
    assert_eq!(
        fetch_usage("openai", &auth("openai-api-key"), None, &client)
            .await
            .unwrap()["available"],
        false
    );
    assert_eq!(
        fetch_usage("github-copilot", &auth("github-copilot"), None, &client)
            .await
            .unwrap()["available"],
        false
    );
    assert!(client.requests.lock().unwrap().is_empty());
    assert!(parsing::unavailable("Usage unavailable")["resetCredits"].is_null());
}

#[tokio::test]
async fn upstream_errors_do_not_expose_provider_response_credentials() {
    let client = client(vec![Reply::status(401)]);
    let error = fetch_usage("anthropic", &auth("anthropic"), None, &client)
        .await
        .unwrap_err();
    assert!(error.contains("401"));
    assert!(!error.contains("private-token"));
}

#[test]
fn malformed_totals_do_not_become_zero_or_leak_unknown_profile_fields() {
    let result = parse_usage(
        "openai",
        &json!({"email":"safe@example.com","profile_user_id":"private-id","credits":{"balance":"NaN"},"rate_limit":{"primary_window":{"used_percent":"Infinity"}}}),
    );
    assert_eq!(result["available"], false);
    assert_eq!(result["account"], json!({"email":"safe@example.com"}));
    assert!(!result.to_string().contains("private-id"));
}

fn parse_usage(plugin: &str, payload: &Value) -> Value {
    match plugin {
        "openai" => crate::openai_provider::usage::parse(payload),
        "xai" => crate::xai_provider::usage::parse(payload),
        "anthropic" => crate::anthropic_provider::usage::parse(payload),
        "github-copilot" => crate::copilot_provider::usage::parse(payload),
        _ => parsing::result(payload, Vec::new(), Vec::new()),
    }
}
