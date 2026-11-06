use nora_ai::{HttpClient, HttpMethod, HttpRequest, ResolvedProviderAuth};
use nora_provider::usage::{parsing::*, request_json};
use serde_json::{json, Value};
use std::time::Duration;

pub fn parse(payload: &Value) -> Value {
    let mut items = Vec::new();
    let mut balances = Vec::new();
    anthropic(payload, &mut items, &mut balances);
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
    if auth.credential_provider_id.as_deref() != Some("anthropic") {
        return Ok(unavailable("This provider does not expose a supported subscription usage endpoint for this account."));
    }
    let request = HttpRequest::new(HttpMethod::Get, "https://api.anthropic.com/api/oauth/usage")
        .with_header("authorization", format!("Bearer {token}"))
        .with_header("anthropic-beta", "oauth-2025-04-20");
    let request = request.with_header("accept", "application/json");
    let payload = request_json(client, request, Duration::from_secs(20)).await?;
    Ok(parse(&payload))
}

fn anthropic(payload: &Value, items: &mut Vec<Value>, balances: &mut Vec<Value>) {
    for (key, label) in [
        ("five_hour", "Five hours"),
        ("seven_day", "Weekly"),
        ("seven_day_opus", "Weekly · Opus"),
        ("seven_day_sonnet", "Weekly · Sonnet"),
        ("seven_day_oauth_apps", "Weekly · OAuth apps"),
        ("seven_day_overage_included", "Weekly · Included overage"),
    ] {
        let window = &payload[key];
        let mut quantities_payload = window.clone();
        if let Some(percent) =
            number(&window["used_percentage"]).or_else(|| number(&window["utilization"]))
        {
            quantities_payload["used_percent"] = json!(percent);
        }
        if let Some(mut item) = quantities(
            key,
            label,
            &quantities_payload,
            None,
            window.get("resets_at"),
        ) {
            item["windowSeconds"] = json!(if key == "five_hour" { 18_000 } else { 604_800 });
            let model_ids = match key {
                "seven_day_opus" => Some(["*opus*"]),
                "seven_day_sonnet" => Some(["*sonnet*"]),
                "seven_day_overage_included" => Some(["*fable*"]),
                _ => None,
            };
            item["countsForRouting"] =
                json!(matches!(key, "five_hour" | "seven_day") || model_ids.is_some());
            if let Some(model_ids) = model_ids {
                item["modelIDs"] = json!(model_ids);
            }
            items.push(item);
        }
    }
    let extra = &payload["extra_usage"];
    if extra["is_enabled"] == true {
        let quantities_payload = json!({"used":number(&extra["used_credits"]),"limit":number(&extra["monthly_limit"]),"used_percent":number(&extra["utilization"])});
        if let Some(mut item) = quantities(
            "extra_usage",
            "Extra usage",
            &quantities_payload,
            Some("credits"),
            extra.get("resets_at"),
        ) {
            item["countsForRouting"] = json!(false);
            remaining_balance(&item, "extra_usage", "Extra usage remaining", balances);
            items.push(item);
        }
    }
}

#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
