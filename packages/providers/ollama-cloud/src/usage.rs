use nora_provider::usage::parsing::{number, timestamp};
use nora_provider_quota::{balance, finish, window};
use serde_json::{json, Value};

pub fn parse(payload: &Value) -> Result<Value, String> {
    let mut items = Vec::new();
    let mut balances = Vec::new();
    let purchased = number(&payload["purchased"]["balance_usd"]);
    let routing = !purchased.is_some_and(|amount| amount > 0.0);
    if let Some(amount) = purchased {
        balances.push(balance("purchased", "Purchased credits", amount, "USD"));
    }
    let included = &payload["included"];
    for (key, label, span) in [
        ("session", "Session limit", None),
        ("weekly", "Weekly", Some(604_800.0)),
    ] {
        let value = &included[key];
        if let Some(left) = number(&value["remaining_percent"]) {
            if let Some(item) = window(
                key,
                label,
                &json!({"used_percent":100.0-left}),
                value.get("resets_at"),
                span,
                routing,
                None,
            ) {
                items.push(item);
            }
        }
    }
    if let (Some(limit), Some(left)) = (
        number(&included["allowance_usd"]).filter(|limit| *limit > 0.0),
        number(&included["balance_usd"]),
    ) {
        let from = timestamp(&included["period"]["from"]);
        let until = timestamp(&included["period"]["until"]);
        let span = from.zip(until).and_then(|(from, until)| {
            let from = chrono::DateTime::parse_from_rfc3339(&from).ok()?;
            let until = chrono::DateTime::parse_from_rfc3339(&until).ok()?;
            Some((until - from).num_seconds() as f64)
        });
        if let Some(item) = window(
            "included",
            "Included allowance",
            &json!({"limit":limit,"used":(limit-left).clamp(0.0,limit)}),
            included["period"].get("until"),
            span,
            routing,
            Some("USD"),
        ) {
            items.push(item);
        }
    }
    finish(items, balances)
}

#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
