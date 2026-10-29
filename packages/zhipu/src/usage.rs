use nora_provider::usage::parsing::number;
use nora_provider_quota::{finish, window};
use serde_json::{json, Value};

pub fn parse(payload: &Value) -> Result<Value, String> {
    if payload["success"] == false
        || payload
            .get("code")
            .and_then(Value::as_i64)
            .is_some_and(|code| code != 200 && code != 0)
    {
        return Err("The GLM plan query was refused.".into());
    }
    let limits = payload["data"]["limits"]
        .as_array()
        .ok_or("The provider did not return a GLM plan.")?;
    let mut items = Vec::new();
    for (index, limit) in limits.iter().enumerate() {
        let Some(percent) = number(&limit["percentage"]) else {
            continue;
        };
        let kind = limit["type"].as_str().unwrap_or_default();
        let unit = limit["unit"].as_i64().unwrap_or_default();
        let hours = number(&limit["number"])
            .filter(|number| *number > 0.0)
            .unwrap_or(5.0);
        let (label, span, routing) = if kind.eq_ignore_ascii_case("TIME_LIMIT") {
            ("MCP monthly allowance".to_string(), None, false)
        } else if unit == 3 {
            (
                format!("{} hours", hours),
                Some(hours * 3600.0),
                (kind.eq_ignore_ascii_case("TOKENS_LIMIT")
                    || kind.eq_ignore_ascii_case("CREDIT_LIMIT")),
            )
        } else if unit == 6 {
            (
                "Weekly".into(),
                Some(604_800.0),
                (kind.eq_ignore_ascii_case("TOKENS_LIMIT")
                    || kind.eq_ignore_ascii_case("CREDIT_LIMIT")),
            )
        } else {
            (
                "Allowance".into(),
                None,
                (kind.eq_ignore_ascii_case("TOKENS_LIMIT")
                    || kind.eq_ignore_ascii_case("CREDIT_LIMIT")),
            )
        };
        if let Some(item) = window(
            &format!("limit:{index}"),
            &label,
            &json!({"used_percent":percent}),
            limit.get("nextResetTime"),
            span,
            routing,
            None,
        ) {
            items.push(item);
        }
    }
    let mut result = finish(items, Vec::new())?;
    if let Some(level) = payload["data"]["level"]
        .as_str()
        .filter(|level| level.len() <= 128)
    {
        result["account"]["plan"] = json!(level);
    }
    Ok(result)
}

#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
