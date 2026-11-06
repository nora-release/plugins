use nora_provider_subscription_core as core;
use serde_json::{json, Value};
use std::collections::BTreeMap;
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse::<f64>().ok()))
        .filter(|n| n.is_finite())
}
fn flatten(value: &Value, depth: usize, out: &mut BTreeMap<String, f64>) {
    if depth > 6 {
        return;
    }
    if let Some(map) = value.as_object() {
        for (key, value) in map {
            if let Some(number) = number(value) {
                out.insert(key.to_lowercase(), number);
            } else {
                flatten(value, depth + 1, out);
            }
        }
    }
}
pub(crate) fn parse(profile: &Value, balance: &Value, limits: &Value, account: &str) -> Value {
    let mut items = Vec::new();
    for (kind, label, span) in [
        ("five_hour", "5 hours", 18000.0),
        ("weekly", "Weekly", 604800.0),
        ("monthly", "Month", 2592000.0),
    ] {
        for limit in limits["limits"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| item["type"] == kind)
        {
            if let Some(percent) = number(&limit["percentUsed"]) {
                items.push(core::usage_item(
                    label,
                    percent.clamp(0.0, 100.0),
                    100.0,
                    core::timestamp(&limit["resetsAt"]),
                    Some(span),
                    true,
                ));
            }
        }
    }
    let mut dollars = number(&balance["balance"]).filter(|n| *n >= 0.0).map(|n| {
        let usd = n / 1_000_000.0;
        if usd == 0.0 || usd >= 0.01 {
            format!("${usd:.2}")
        } else {
            format!("${usd:.4}")
        }
    });
    if dollars.is_none() {
        let mut flat = BTreeMap::new();
        flatten(balance, 0, &mut flat);
        let pick = |keys: &[&str]| keys.iter().find_map(|key| flat.get(*key).copied());
        let total = pick(&[
            "total",
            "totalcredits",
            "totalbalance",
            "creditstotal",
            "limit",
            "amount",
            "quota",
        ]);
        let used = pick(&[
            "used",
            "usedcredits",
            "usedamount",
            "spent",
            "consumed",
            "usagetotal",
        ]);
        let left = pick(&[
            "remaining",
            "remainingcredits",
            "remainingbalance",
            "left",
            "available",
            "balance",
            "credits",
            "current",
        ]);
        if let Some(total) = total.filter(|n| *n > 0.0) {
            if let Some(used) = used.or_else(|| left.filter(|n| *n <= total).map(|n| total - n)) {
                let reset = [
                    "resetAt",
                    "resetTime",
                    "nextResetAt",
                    "renewsAt",
                    "expiresAt",
                ]
                .iter()
                .find_map(|key| core::timestamp(&balance[key]));
                items.push(core::usage_item("Credits", used, total, reset, None, true));
            }
        } else if let Some(left) = left.filter(|n| *n >= 0.0) {
            dollars = Some(format!("{left} credits"));
        } else if let Some(used) = used.filter(|n| *n > 0.0) {
            items.push(json!({"label":"Credits","value":format!("{used} credits used"),"countsForRouting":false}));
        }
    }
    let plan = ["plan", "planType", "planName", "membership", "tier"]
        .iter()
        .find_map(|key| core::text(&profile[key]));
    json!({"available":!items.is_empty()||dollars.is_some(),"account":{"id":account,"email":profile["email"],"name":profile["name"],"plan":plan},"balance":dollars,"items":items})
}
#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
