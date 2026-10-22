use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{json, Value};

pub fn result(payload: &Value, items: Vec<Value>, balances: Vec<Value>) -> Value {
    let mut result = json!({"available":false,"items":items,"account":account(payload),"balances":balances,"resetCredits":null,"updatedAt":Utc::now().to_rfc3339()});
    if let Some(plan) = result["account"].get("plan") {
        result["plan"] = plan.clone();
    }
    update_availability(&mut result);
    result
}

pub fn update_availability(result: &mut Value) {
    let available = ["items", "balances"].iter().any(|key| {
        result[*key]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    }) || result["resetCredits"]["available"] == true;
    result["available"] = json!(available);
    if available {
        result.as_object_mut().unwrap().remove("message");
    } else {
        result["message"] = json!("The provider did not return usage totals for this account.");
    }
}

pub fn unavailable(message: &str) -> Value {
    json!({"available":false,"message":message,"items":[],"account":{},"balances":[],
        "resetCredits":null,
        "updatedAt":Utc::now().to_rfc3339()})
}

pub fn account(payload: &Value) -> Value {
    let mut result = json!({});
    for (key, paths) in [
        ("email", &["/email", "/account/email", "/user/email"][..]),
        (
            "name",
            &["/name", "/display_name", "/account/name", "/user/name"][..],
        ),
        (
            "plan",
            &[
                "/plan_type",
                "/subscriptionTier",
                "/copilot_plan",
                "/account/plan",
            ][..],
        ),
    ] {
        if let Some(value) = paths.iter().find_map(|path| {
            payload
                .pointer(path)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        }) {
            result[key] = json!(value);
        }
    }
    result
}

pub fn quantities(
    id: &str,
    label: &str,
    payload: &Value,
    unit: Option<&str>,
    resets: Option<&Value>,
) -> Option<Value> {
    let limit = number(&payload["limit"]);
    let remaining = number(&payload["remaining"]);
    let used = number(&payload["used"]).or_else(|| Some((limit? - remaining?).max(0.0)));
    let remaining = remaining.or_else(|| Some((limit? - used?).max(0.0)));
    let percent = number(&payload["used_percent"])
        .or_else(|| (limit? > 0.0).then_some(used? / limit? * 100.0));
    if used.is_none() && limit.is_none() && remaining.is_none() && percent.is_none() {
        return None;
    }
    let value = match (used, limit, percent) {
        (Some(used), Some(limit), _) => format!(
            "{} / {}{}",
            compact(used),
            compact(limit),
            unit.map(|unit| format!(" {unit}")).unwrap_or_default()
        ),
        (_, _, Some(percent)) => format!("{:.0}% used", percent.clamp(0.0, 100.0)),
        (_, _, _) => "Usage totals".into(),
    };
    let mut item = json!({"id":id,"label":label,"value":value});
    put_number(&mut item, "used", used);
    put_number(&mut item, "limit", limit);
    put_number(&mut item, "remaining", remaining);
    put_number(
        &mut item,
        "usedPercent",
        percent.map(|value| value.clamp(0.0, 100.0)),
    );
    if let Some(unit) = unit {
        item["unit"] = json!(unit);
    }
    if let Some(resets) = resets.and_then(timestamp) {
        item["resetsAt"] = json!(resets);
    }
    Some(item)
}

pub fn remaining_balance(item: &Value, id: &str, label: &str, balances: &mut Vec<Value>) {
    if let Some(amount) = number(&item["remaining"]) {
        let mut balance = json!({"id":id,"label":label,"amount":amount});
        if let Some(unit) = item.get("unit") {
            balance["unit"] = unit.clone();
        }
        balances.push(balance);
    }
}

pub fn timestamp(value: &Value) -> Option<String> {
    if let Some(value) = value.as_str() {
        if let Ok(date) = DateTime::parse_from_rfc3339(value) {
            return Some(date.to_rfc3339());
        }
        if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
            return Some(date.and_hms_opt(0, 0, 0)?.and_utc().to_rfc3339());
        }
    }
    let value = number(value)?;
    if value <= 0.0 || value > i64::MAX as f64 {
        return None;
    }
    let millis = if value < 100_000_000_000.0 {
        value * 1000.0
    } else {
        value
    };
    DateTime::from_timestamp_millis(millis as i64).map(|date| date.to_rfc3339())
}

pub fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.trim().parse().ok()))
        .or_else(|| value.get("val").and_then(number))
        .filter(|value| value.is_finite())
}

pub fn put_number(value: &mut Value, key: &str, number: Option<f64>) {
    if let Some(number) = number {
        value[key] = json!(number);
    }
}

pub fn text<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
    })
}

pub fn compact(value: f64) -> String {
    format!("{value:.2}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

#[cfg(test)]
pub(super) fn parse_usage(plugin: &str, payload: &Value) -> Value {
    match plugin {
        "openai" => crate::openai_provider::usage::parse(payload),
        "xai" => crate::xai_provider::usage::parse(payload),
        "anthropic" => crate::anthropic_provider::usage::parse(payload),
        "github-copilot" => crate::copilot_provider::usage::parse(payload),
        _ => result(payload, Vec::new(), Vec::new()),
    }
}
