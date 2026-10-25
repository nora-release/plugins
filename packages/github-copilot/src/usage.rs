use nora_ai::{HttpClient, HttpMethod, HttpRequest, ResolvedProviderAuth};
use nora_provider::usage::{parsing::*, request_json};
use serde_json::{json, Value};
use std::time::Duration;

pub fn parse(payload: &Value) -> Value {
    let mut items = Vec::new();
    let mut balances = Vec::new();
    copilot(payload, &mut items, &mut balances);
    let result = result(payload, items, balances);
    result
}

pub async fn fetch(
    auth: &ResolvedProviderAuth,
    _account_id: Option<&str>,
    client: &dyn HttpClient,
) -> Result<Value, String> {
    let Some(token) = auth
        .token
        .as_deref()
        .filter(|token| !token.trim().is_empty())
    else {
        return Ok(unavailable("Sign in to this provider to read its usage."));
    };
    let github_token = ["githubOAuthToken", "githubToken"]
        .into_iter()
        .find_map(|key| auth.metadata.get(key).and_then(|value| value.as_str()))
        .or_else(|| {
            ["gho_", "ghu_", "ghp_"]
                .iter()
                .any(|prefix| token.starts_with(prefix))
                .then_some(token)
        });
    let Some(github_token) = github_token else {
        return Ok(unavailable("Copilot usage requires the GitHub OAuth account token, which is separate from its chat token."));
    };
    let host = match auth.metadata.get("githubHost") {
        Some(host) => host
            .as_str()
            .and_then(nora_provider_auth::copilot_identity::normalize_host)
            .ok_or_else(|| "The GitHub account host is invalid.".to_string())?,
        None => String::new(),
    };
    let mut request = HttpRequest::new(
        HttpMethod::Get,
        nora_provider_auth::copilot_identity::api_url(&host, "/copilot_internal/user"),
    )
    .with_header("authorization", format!("token {github_token}"));
    request
        .headers
        .extend(nora_provider_auth::copilot_client::editor_headers());
    let request = request.with_header("accept", "application/json");
    let payload = request_json(client, request, Duration::from_secs(20)).await?;
    Ok(parse(&payload))
}

fn copilot(payload: &Value, items: &mut Vec<Value>, balances: &mut Vec<Value>) {
    let managed = payload["copilot_plan"].as_str().is_some_and(|plan| {
        plan.eq_ignore_ascii_case("business") || plan.eq_ignore_ascii_case("enterprise")
    });
    for (key, label) in [
        ("premium_interactions", "Premium requests"),
        ("chat", "Chat requests"),
        ("completions", "Completions"),
    ] {
        let quota = &payload["quota_snapshots"][key];
        if !quota.is_object() {
            continue;
        }
        let resets = quota
            .get("quota_reset_at")
            .filter(|value| timestamp(value).is_some())
            .or_else(|| {
                payload
                    .get("quota_reset_date_utc")
                    .filter(|value| timestamp(value).is_some())
            })
            .or_else(|| {
                payload
                    .get("quota_reset_date")
                    .filter(|value| timestamp(value).is_some())
            });
        let exhausted_pool = key == "premium_interactions"
            && quota["unlimited"] == true
            && quota["has_quota"] == false;
        if quota["unlimited"] == true && !exhausted_pool {
            let mut item =
                json!({"id":key,"label":label,"value":"Unlimited","countsForRouting":false});
            if let Some(resets) = resets.and_then(timestamp) {
                item["resetsAt"] = json!(resets);
            }
            items.push(item);
            balances.push(json!({"id":key,"label":label,"unlimited":true,"unit":"requests"}));
            continue;
        }
        let limit = number(&quota["entitlement"]);
        if !exhausted_pool && limit.is_some_and(|limit| limit <= 0.0) {
            continue;
        }
        let remaining = number(&quota["quota_remaining"]).or_else(|| number(&quota["remaining"]));
        let percent = if exhausted_pool {
            Some(100.0)
        } else if let (Some(limit), Some(remaining)) =
            (limit.filter(|limit| *limit > 0.0), remaining)
        {
            Some((limit - remaining).max(0.0) / limit * 100.0)
        } else {
            number(&quota["percent_remaining"]).map(|remaining| 100.0 - remaining)
        };
        let quantities_payload =
            json!({"limit":limit,"remaining":remaining,"used_percent":percent});
        if let Some(mut item) =
            quantities(key, label, &quantities_payload, Some("requests"), resets)
        {
            item["countsForRouting"] = json!(
                (key == "chat"
                    || key == "premium_interactions"
                        && (managed || quota["overage_permitted"] != true))
                    && (exhausted_pool || limit.is_none_or(|limit| limit > 0.0))
            );
            remaining_balance(&item, key, label, balances);
            items.push(item);
        }
    }
}

#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
