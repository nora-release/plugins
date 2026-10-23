use super::{account::Account, models};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
fn end(value: &Value) -> Option<Value> {
    let ms = if let Some(n) = value.as_u64() {
        Some(n)
    } else {
        value.as_str().and_then(|s| {
            s.parse::<u64>().ok().or_else(|| {
                chrono::DateTime::parse_from_rfc3339(s)
                    .ok()
                    .and_then(|d| u64::try_from(d.timestamp_millis()).ok())
            })
        })
    }?;
    if ms == 0 {
        return None;
    }
    let value = nora_provider_subscription_core::iso(ms);
    (!value.is_null()).then_some(value)
}
pub(crate) fn parse(value: &Value, account: &Account, id: &str) -> PluginResult<Value> {
    if !value
        .pointer("/limits/standard")
        .is_some_and(Value::is_object)
    {
        return Err(PluginError::transport(
            "Factory returned no standard billing windows.",
        ));
    }
    let cents = value["extraUsageBalanceCents"]
        .as_f64()
        .filter(|v| v.is_finite());
    let extra = value["extraUsageAllowed"] == true && cents.is_some_and(|n| n > 0.);
    let mut items = Vec::new();
    for (group, label, is_core) in [("standard", "", false), ("core", "Droid Core · ", true)] {
        for (key, name, seconds) in [
            ("fiveHour", "5 hours", 18000),
            ("weekly", "7 days", 604800),
            ("monthly", "30 days", 2592000),
        ] {
            let window = &value["limits"][group][key];
            if !window.is_object() {
                continue;
            }
            let used = window["usedPercent"]
                .as_f64()
                .filter(|n| n.is_finite())
                .ok_or_else(|| {
                    PluginError::transport("Factory returned an invalid billing percentage.")
                })?
                .clamp(0., 100.);
            let mut item = json!({"id":format!("{group}-{key}"),"label":format!("{label}{name}"),"type":"quota","used":used,"limit":100,"remaining":100.-used,"usedPercent":used,"value":format!("{used:.0}% used"),"unit":"percent","windowSeconds":seconds,"countsForRouting":!extra,"modelIDs":models::ids(is_core)});
            if let Some(reset) = end(&window["windowEnd"]) {
                item["resetsAt"] = reset;
            }
            items.push(item);
        }
    }
    if let Some(cents) = cents.filter(|n| *n > 0.) {
        items.push(json!({"id":"extra-usage","label":"Extra usage","type":"balance","balance":cents/100.,"value":format!("${:.2}",cents/100.),"currency":"USD","countsForRouting":false}));
    }
    if items.is_empty() {
        return Err(PluginError::transport(
            "Factory returned no billing windows.",
        ));
    }
    Ok(nora_provider::usage::parsing::result(
        &json!({"account":{"id":id,"name":if account.email.is_empty(){&account.user}else{&account.email}}}),
        items,
        Vec::new(),
    ))
}
#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
