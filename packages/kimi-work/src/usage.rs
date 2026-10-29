use nora_provider::usage::parsing::{quantities, result};
use serde_json::{json, Value};

pub(super) fn parse(payload: &Value) -> Value {
    let mut items = Vec::new();
    let used = payload
        .pointer("/subscriptionBalance/amountUsedRatio")
        .and_then(Value::as_f64)
        .filter(|used| used.is_finite());
    if let Some(used) = used {
        if let Some(mut item) = quantities(
            "shared_membership",
            "Shared membership allowance",
            &json!({"used_percent":used.clamp(0.0, 1.0) * 100.0}),
            None,
            None,
        ) {
            item["countsForRouting"] = json!(true);
            items.push(item);
        }
    }
    let mut usage = result(&json!({}), items, Vec::new());
    usage["account"] = json!({"plan":"Kimi Work · Shared membership allowance"});
    usage
}

#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
