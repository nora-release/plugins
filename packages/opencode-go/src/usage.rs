use nora_provider::usage::parsing::number;
use nora_provider_quota::{finish, window};
use serde_json::{json, Value};

pub fn parse(payload: &Value) -> Result<Value, String> {
    let mut items = Vec::new();
    for (key, label, span) in [
        ("rolling", "Five hours", Some(18_000.0)),
        ("weekly", "Weekly", Some(604_800.0)),
        ("monthly", "Monthly", None),
    ] {
        let value = &payload["usage"][key];
        let Some(percent) = number(&value["percent"]) else {
            continue;
        };
        if let Some(item) = window(
            key,
            label,
            &json!({"used_percent":percent}),
            if percent > 0.0 {
                value.get("resetsAt")
            } else {
                None
            },
            span,
            true,
            None,
        ) {
            items.push(item);
        }
    }
    finish(items, Vec::new())
}

#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
