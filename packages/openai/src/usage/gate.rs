//! ChatGPT's allowance and purchased-credit gate, adapted from Magpie (MIT).
use nora_provider::usage::parsing::{number, timestamp};
use serde_json::{json, Value};

const WORKSPACE_PLANS: &[&str] = &[
    "free_workspace",
    "team",
    "self_serve_business_prolite",
    "self_serve_business_usage_based",
    "business",
    "ent26",
    "enterprise_cbp_automation",
    "enterprise_cbp_trial",
    "enterprise_cbp_usage_based",
    "enterprise_cbp_view_only",
    "enterprise",
    "hc",
    "finserv",
    "law",
    "sci",
    "education",
    "edu",
    "edu_plus",
    "edu_pro",
    "quorum",
    "k12",
];
const RESERVE_PLANS: &[&str] = &["team", "self_serve_business_prolite"];

fn credits(payload: &Value) -> bool {
    if payload
        .pointer("/credits/unlimited")
        .and_then(Value::as_bool)
        == Some(true)
    {
        return true;
    }
    if payload
        .pointer("/credits/has_credits")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return false;
    }
    payload
        .pointer("/credits/balance")
        .and_then(|balance| {
            balance
                .as_f64()
                .or_else(|| balance.as_str()?.trim().parse::<f64>().ok())
        })
        .is_none_or(|balance| balance > 0.0)
}
fn overage(payload: &Value) -> bool {
    let plan = payload
        .get("plan_type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    WORKSPACE_PLANS.contains(&plan.as_str())
        && !RESERVE_PLANS.contains(&plan.as_str())
        && payload
            .pointer("/credits/overage_limit_reached")
            .and_then(Value::as_bool)
            == Some(false)
        && payload
            .pointer("/spend_control/reached")
            .and_then(Value::as_bool)
            != Some(true)
        && payload
            .pointer("/rate_limit_reached_type/type")
            .and_then(Value::as_str)
            .is_none_or(|kind| kind == "rate_limit_reached")
}
pub(crate) fn held(payload: &Value) -> bool {
    payload
        .pointer("/rate_limit/allowed")
        .and_then(Value::as_bool)
        == Some(false)
        && !credits(payload)
        && !overage(payload)
}

pub(crate) fn apply(payload: &Value, result: &mut Value) {
    let held = held(payload);
    let mut reset = None;
    if let Some(items) = result.get_mut("items").and_then(Value::as_array_mut) {
        for item in items {
            if !item
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| id.starts_with("rate_limit:"))
            {
                continue;
            }
            item["includedAllowance"] = json!(true);
            let full = item
                .get("usedPercent")
                .and_then(number)
                .is_some_and(|used| used >= 100.0);
            // A full included allowance is still displayed when credits or the
            // account's overage carry requests on. It must not cool the account.
            item["countsForRouting"] = json!(!full || held);
            if held && full {
                if let Some(at) = item.get("resetsAt").and_then(timestamp) {
                    if reset.as_ref().is_none_or(|old: &String| at > *old) {
                        reset = Some(at);
                    }
                }
            }
        }
    }
    let mut routing = json!({"allowed":!held,"supportsExtraBalance":true,"extraBalanceAvailable":credits(payload)||overage(payload)});
    if held {
        routing["message"] = json!("This provider account cannot accept requests until its allowance becomes available again.");
        if let Some(reset) = reset {
            routing["resetsAt"] = json!(reset);
        }
    }
    result["routing"] = routing;
}

#[cfg(test)]
#[path = "../../test/gate.test.rs"]
mod tests;
