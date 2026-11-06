use super::{
    account::Account,
    models::{self, Model},
};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| {
            value
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .and_then(|s| s.parse().ok())
        })
        .filter(|v| v.is_finite())
}
fn reset(value: &Value) -> Option<Value> {
    nora_provider_subscription_core::timestamp(value)
        .or_else(|| {
            value.as_str().and_then(|s| s.parse::<u64>().ok()).map(|n| {
                if n < 100000000000 {
                    n * 1000
                } else {
                    n
                }
            })
        })
        .map(nora_provider_subscription_core::iso)
        .filter(|v| !v.is_null())
}
pub(crate) fn parse(
    value: &Value,
    account: &Account,
    id: &str,
    rows: &[Model],
) -> PluginResult<Value> {
    let status = value
        .pointer("/userStatus/planStatus")
        .filter(|v| v.is_object())
        .ok_or_else(|| PluginError::transport("Devin returned no account plan status."))?;
    let info = &status["planInfo"];
    let quota = info["billingStrategy"] == "BILLING_STRATEGY_QUOTA";
    let mut items = Vec::new();
    let paid = models::paid_ids(rows);
    for (key, label, period, percent, end, hide) in [
        (
            "daily",
            "1 day",
            86400,
            "dailyQuotaRemainingPercent",
            "dailyQuotaResetAtUnix",
            "hideDailyQuota",
        ),
        (
            "weekly",
            "7 days",
            604800,
            "weeklyQuotaRemainingPercent",
            "weeklyQuotaResetAtUnix",
            "hideWeeklyQuota",
        ),
    ] {
        if info[hide] == true {
            continue;
        }
        let expires = reset(&status[end]);
        let raw = status.get(percent).filter(|v| !v.is_null());
        if raw.is_none() && !(quota && expires.is_some()) {
            continue;
        }
        let left = if let Some(raw) = raw {
            number(raw).ok_or_else(|| {
                PluginError::transport("Devin returned an invalid remaining quota.")
            })?
        } else {
            0.
        };
        let used = (100. - left).clamp(0., 100.);
        let mut item = json!({"id":key,"label":label,"used":used,"limit":100,"remaining":100.-used,"usedPercent":used,"value":format!("{used:.0}% used"),"windowSeconds":period,"modelIDs":paid,"countsForRouting":!paid.is_empty()});
        if let Some(expires) = expires {
            item["resetsAt"] = expires;
        }
        items.push(item);
    }
    if let Some(limit) = number(&status["acuLimit"]).filter(|n| *n > 0.) {
        let used = number(&status["acuConsumed"]).unwrap_or(0.);
        let mut item = json!({"id":"acus","label":"ACUs","used":used,"limit":limit,"remaining":(limit-used).max(0.),"usedPercent":(used/limit*100.).clamp(0.,100.),"value":format!("{used:.2} / {limit:.2} ACUs"),"countsForRouting":false});
        if let Some(expires) = reset(&status["planEnd"]) {
            item["resetsAt"] = expires;
        }
        items.push(item);
    }
    let mut balances = Vec::new();
    if let Some(micros) = number(&status["overageBalanceMicros"]).filter(|v| *v > 0.) {
        balances.push(json!({"id":"extra","label":"Extra usage","balance":micros/1000000.,"currency":"USD","value":format!("${:.2}",micros/1000000.),"countsForRouting":false}));
    }
    if items.is_empty() && balances.is_empty() {
        items.push(json!({"id":"plan","label":"Plan","value":if account.profile.plan.is_empty(){"Devin"}else{&account.profile.plan},"countsForRouting":false}));
    }
    let mut result = nora_provider::usage::parsing::result(
        &json!({"account":account.profile.public(id)}),
        items,
        balances,
    );
    result["account"] = account.profile.public(id);
    if let Some(until) = reset(&status["planEnd"]) {
        result["expiresAt"] = until;
    }
    Ok(result)
}
#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
