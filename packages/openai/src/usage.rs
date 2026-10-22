use chrono::Utc;
use nora_ai::{HttpClient, HttpMethod, HttpRequest, ResolvedProviderAuth};
use nora_provider::usage::{header, parsing::*, request_json};
use serde_json::{json, Value};
use std::time::Duration;
#[path = "usage/resets.rs"]
pub mod resets;

pub fn parse(payload: &Value) -> Value {
    let mut items = Vec::new();
    let mut balances = Vec::new();
    openai(payload, &mut items, &mut balances);
    let mut result = result(payload, items, balances);
    result["resetCredits"] = resets::from_usage(payload);
    update_availability(&mut result);
    result
}

pub async fn fetch(
    auth: &ResolvedProviderAuth,
    account_id: Option<&str>,
    client: &dyn HttpClient,
) -> Result<Value, String> {
    let Some(token) = auth
        .token
        .as_deref()
        .filter(|token| !token.trim().is_empty())
    else {
        return Ok(unavailable("Sign in to this provider to read its usage."));
    };
    if auth.credential_provider_id.as_deref() != Some("openai-codex") {
        return Ok(unavailable("This provider does not expose a supported subscription usage endpoint for this account."));
    }
    let Some(account) = account_id
        .filter(|id| !id.is_empty())
        .or_else(|| header(auth, "chatgpt-account-id"))
    else {
        return Ok(unavailable(
            "The ChatGPT account ID is required to read its usage.",
        ));
    };
    let request = HttpRequest::new(
        HttpMethod::Get,
        "https://chatgpt.com/backend-api/wham/usage",
    )
    .with_header("authorization", format!("Bearer {token}"))
    .with_header("chatgpt-account-id", account)
    .with_header("user-agent", "codex-cli")
    .with_header("origin", "https://chatgpt.com")
    .with_header("referer", "https://chatgpt.com/");
    let request = request.with_header("accept", "application/json");
    let mut reset_request = request.clone();
    reset_request.url = "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits".into();
    let payload = request_json(client, request, Duration::from_secs(20)).await?;
    let mut result = parse(&payload);
    match request_json(client, reset_request, Duration::from_secs(4)).await {
        Ok(payload) => {
            let mut details = resets::parse(&payload);
            if details["available"] == true {
                if resets::available_count(&payload).is_none() {
                    if let Some(count) = result["resetCredits"].get("totalAvailable") {
                        details["totalAvailable"] = count.clone();
                    }
                }
                result["resetCredits"] = details;
            } else {
                result["resetCredits"]["message"] =
                    json!("Reset credit details were not provided.");
            }
        }
        Err(_) => {
            result["resetCredits"]["message"] =
                json!("Reset credit details could not be refreshed.");
        }
    }
    update_availability(&mut result);
    Ok(result)
}

fn openai(payload: &Value, items: &mut Vec<Value>, balances: &mut Vec<Value>) {
    openai_windows(&payload["rate_limit"], "rate_limit", "", items);
    openai_windows(
        &payload["code_review_rate_limit"],
        "code_review",
        "Code review",
        items,
    );
    match &payload["additional_rate_limits"] {
        Value::Array(limits) => {
            for (index, limit) in limits.iter().enumerate() {
                let id = text(limit, &["metered_feature", "limit_name", "id"])
                    .map(str::to_owned)
                    .unwrap_or_else(|| index.to_string());
                let label = text(limit, &["limit_name", "name", "metered_feature"])
                    .unwrap_or("Additional allowance");
                let windows = limit.get("rate_limit").unwrap_or(limit);
                openai_windows(windows, &format!("additional:{id}"), label, items);
            }
        }
        Value::Object(limits) => {
            for (id, limit) in limits {
                let label = text(limit, &["limit_name", "name"]).unwrap_or(id);
                openai_windows(
                    limit.get("rate_limit").unwrap_or(limit),
                    &format!("additional:{id}"),
                    label,
                    items,
                );
            }
        }
        _ => {}
    }
    let credits = &payload["credits"];
    if credits.is_object() {
        let amount = number(&credits["balance"]);
        let unlimited = credits["unlimited"].as_bool();
        if amount.is_some() || unlimited.is_some() {
            let mut balance = json!({"id":"credits","label":"Credits","unit":"credits"});
            put_number(&mut balance, "amount", amount);
            if let Some(unlimited) = unlimited {
                balance["unlimited"] = json!(unlimited);
            }
            balances.push(balance);
        }
    }
    let spend = &payload["spend_control"]["individual_limit"];
    if spend.is_object() {
        if let Some(item) = quantities(
            "spend_control",
            "Spending limit",
            spend,
            Some("credits"),
            spend.get("reset_at"),
        ) {
            items.push(item);
        }
    }
}

fn openai_windows(limits: &Value, id: &str, prefix: &str, items: &mut Vec<Value>) {
    for (key, fallback) in [
        ("primary_window", "Primary window"),
        ("secondary_window", "Secondary window"),
    ] {
        let window = &limits[key];
        let label = match number(&window["limit_window_seconds"]) {
            Some(seconds) if seconds == 18000.0 => "Five hours".to_string(),
            Some(seconds) if seconds == 604800.0 => "Weekly".to_string(),
            Some(seconds) if seconds > 0.0 && seconds % 3600.0 == 0.0 => {
                format!("{} hours", compact(seconds / 3600.0))
            }
            _ => fallback.into(),
        };
        let label = if prefix.is_empty() {
            label
        } else {
            format!("{prefix} · {label}")
        };
        let resets = window.get("reset_at").and_then(timestamp).or_else(|| {
            let seconds = number(&window["reset_after_seconds"])?;
            (seconds >= 0.0 && seconds < i64::MAX as f64 / 1000.0).then(|| {
                (Utc::now() + chrono::Duration::milliseconds((seconds * 1000.0) as i64))
                    .to_rfc3339()
            })
        });
        if let Some(mut item) = quantities(&format!("{id}:{key}"), &label, window, None, None) {
            if let Some(resets) = resets {
                item["resetsAt"] = json!(resets);
            }
            items.push(item);
        }
    }
}
