use nora_provider::usage::parsing::{number, timestamp};
use nora_provider_quota::{finish, window};
use serde_json::Value;

pub fn parse(payload: &Value) -> Result<Value, String> {
    let mut items = Vec::new();
    for (index, limit) in payload["limits"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let spec = &limit["window"];
        let count = number(&spec["duration"]).filter(|value| *value > 0.0);
        let unit = spec["timeUnit"].as_str().unwrap_or_default();
        let multiplier = if unit.contains("MINUTE") {
            Some(60.0)
        } else if unit.contains("HOUR") {
            Some(3600.0)
        } else if unit.contains("DAY") {
            Some(86_400.0)
        } else {
            None
        };
        let span = count
            .zip(multiplier)
            .map(|(count, multiplier)| count * multiplier);
        let detail = limit
            .get("detail")
            .filter(|value| value.is_object())
            .unwrap_or(spec);
        add(
            &mut items,
            &format!("limit:{index}"),
            "Allowance",
            detail,
            span,
        );
    }
    add(
        &mut items,
        "weekly",
        "Weekly",
        &payload["usage"],
        Some(604_800.0),
    );
    finish(items, Vec::new())
}
fn add(items: &mut Vec<Value>, id: &str, label: &str, detail: &Value, span: Option<f64>) {
    if !number(&detail["limit"]).is_some_and(|limit| limit > 0.0)
        || number(&detail["used"])
            .or_else(|| number(&detail["remaining"]))
            .is_none()
    {
        return;
    }
    let reset = ["resetTime", "resetAt", "reset_at", "reset_time"]
        .into_iter()
        .find_map(|key| detail.get(key).filter(|value| timestamp(value).is_some()));
    if let Some(item) = window(id, label, detail, reset, span, true, None) {
        items.push(item);
    }
}

#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
