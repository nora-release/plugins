use nora_ai::{HttpClient, ResolvedProviderAuth};
use nora_provider::usage::{header, parsing::*, request_json};
use serde_json::{json, Value};
use std::time::Duration;

pub fn parse(payload: &Value) -> Value {
    let mut items = Vec::new();
    let mut balances = Vec::new();
    grok(payload, &mut items, &mut balances);
    let result = result(payload, items, balances);
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
    if auth.credential_provider_id.as_deref() != Some("xai") {
        return Ok(unavailable("This provider does not expose a supported subscription usage endpoint for this account."));
    }
    let Some(account) = account_id
        .filter(|id| !id.is_empty())
        .or_else(|| header(auth, "x-userid"))
    else {
        return Ok(unavailable(
            "The verified Grok account ID is required to read its usage.",
        ));
    };
    let mut request = nora_provider_auth::xai_oauth_billing_request(token, account)
        .map_err(|_| "The Grok account could not be used to read its usage")?;
    for key in [
        nora_provider_auth::XAI_CLIENT_VERSION_HEADER,
        nora_provider_auth::XAI_CLIENT_MODE_HEADER,
    ] {
        if let Some(value) = header(auth, key) {
            request.headers.insert(key.into(), value.into());
        }
    }
    let request = request.with_header("accept", "application/json");
    let payload = request_json(client, request, Duration::from_secs(20)).await?;
    Ok(parse(&payload))
}

fn grok(payload: &Value, items: &mut Vec<Value>, balances: &mut Vec<Value>) {
    let config = &payload["config"];
    let resets = config["currentPeriod"]
        .get("end")
        .or_else(|| config.get("billingPeriodEnd"));
    let mut allowance = json!({});
    put_number(
        &mut allowance,
        "used_percent",
        number(&config["creditUsagePercent"]),
    );
    put_number(&mut allowance, "used", number(&config["used"]));
    put_number(&mut allowance, "limit", number(&config["monthlyLimit"]));
    let label = match config["currentPeriod"]["type"].as_str() {
        Some("USAGE_PERIOD_TYPE_DAILY" | "DAILY") => "Daily credits",
        Some("USAGE_PERIOD_TYPE_WEEKLY" | "WEEKLY") => "Weekly credits",
        Some("USAGE_PERIOD_TYPE_MONTHLY" | "MONTHLY") => "Monthly credits",
        _ => "Grok credits",
    };
    if let Some(item) = quantities(
        "included_credits",
        label,
        &allowance,
        Some("credits"),
        resets,
    ) {
        remaining_balance(
            &item,
            "included_credits",
            "Included credits remaining",
            balances,
        );
        items.push(item);
    }
    let on_demand =
        json!({"used":number(&config["onDemandUsed"]),"limit":number(&config["onDemandCap"])});
    if let Some(item) = quantities(
        "on_demand",
        "On-demand credits",
        &on_demand,
        Some("credits"),
        resets,
    ) {
        remaining_balance(
            &item,
            "on_demand",
            "On-demand allowance remaining",
            balances,
        );
        items.push(item);
    }
    if let Some(amount) = number(&config["prepaidBalance"]) {
        balances.push(json!({"id":"prepaid_credits","label":"Prepaid credits","amount":amount,"unit":"credits"}));
    }
}
