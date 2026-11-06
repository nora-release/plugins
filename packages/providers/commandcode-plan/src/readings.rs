use nora_provider_subscription_core as core;
use serde_json::{json, Value};
pub(crate) fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.trim().parse().ok()))
        .filter(|n| n.is_finite())
}
pub(crate) fn plan(id: &str) -> (&str, f64) {
    let id = id.to_lowercase().replace('_', "-");
    for (prefix, name, credits) in [
        ("individual-provider", "Provider", 15.),
        ("individual-pro-v1", "Pro", 80.),
        ("individual-goat", "GOAT", 70.),
        ("individual-ultra", "Ultra", 300.),
        ("individual-max", "Max", 150.),
        ("individual-pro", "Pro", 30.),
        ("individual-go", "Go", 10.),
        ("teams-pro", "Teams Pro", 40.),
    ] {
        if id.starts_with(prefix) {
            return (name, credits);
        }
    }
    ("", 0.)
}
pub(crate) fn subscription(value: &Value) -> Option<Value> {
    if value["success"] == false {
        return None;
    }
    let data = &value["data"];
    if let Some(id) = core::text(&data["planId"]).filter(|_| {
        ![Some("canceled"), Some("incomplete_expired")].contains(&data["status"].as_str())
    }) {
        let (name, _) = plan(id);
        Some(
            json!({"id":id,"plan":if name.is_empty(){id}else{name},"until":core::timestamp(&data["currentPeriodEnd"]),"renew":data["cancelAtPeriodEnd"].as_bool().map(|cancel|if cancel{"off"}else{"auto"})}),
        )
    } else {
        Some(json!({"id":"","plan":"No plan"}))
    }
}
pub(crate) fn extra(value: &Value) -> bool {
    ["purchasedCredits", "freeCredits"]
        .iter()
        .any(|key| number(&value["credits"][key]).is_some_and(|n| n > 0.))
}
pub(crate) fn usage(value: Option<&Value>, sub: Option<&Value>, account: &str) -> Value {
    let mut items = Vec::new();
    let mut balance = None;
    let mut name = sub
        .and_then(|s| core::text(&s["plan"]))
        .unwrap_or("")
        .to_owned();
    if let Some(value) = value {
        for (label, key, span) in [
            ("5 hours", "fiveHour", 5. * 3600.),
            ("Weekly", "weekly", 7. * 24. * 3600.),
        ] {
            let window = &value["windowLimits"][key];
            if let Some((used, limit)) = number(&window["used"])
                .zip(number(&window["cap"]))
                .filter(|(_, limit)| *limit > 0.)
            {
                items.push(core::usage_item(
                    label,
                    used.max(0.).min(limit),
                    limit,
                    core::timestamp(&window["resetAt"]),
                    Some(span),
                    true,
                ));
            }
        }
        let id = core::text(&value["credits"]["planId"])
            .or_else(|| sub.and_then(|s| core::text(&s["id"])))
            .unwrap_or("");
        let (plan, monthly) = plan(id);
        if name.is_empty() {
            name = plan.to_owned();
        }
        let mut left = 0.;
        let mut month = 0.;
        let mut known = false;
        for (index, key) in ["monthlyCredits", "purchasedCredits", "freeCredits"]
            .iter()
            .enumerate()
        {
            if let Some(n) = number(&value["credits"][key]) {
                let n = n.max(0.);
                left += n;
                known = true;
                if index == 0 {
                    month = n;
                }
            }
        }
        if known && monthly > 0. {
            let pool = monthly.max(month) + left - month;
            if pool > 0. {
                let reset = sub.and_then(|s| s["until"].as_u64());
                let mut item =
                    core::usage_item("Credits", (pool - left).max(0.), pool, reset, None, true);
                item["display"] = json!(format!("${:.2} / ${pool:.2}", (pool - left).max(0.)));
                items.push(item);
            }
        } else if known && items.is_empty() {
            balance = Some(format!("${left:.2}"));
        }
    }
    json!({"available":value.is_some(),"account":{"id":account,"plan":name},"plan":name,"until":sub.and_then(|s|s["until"].as_u64()).map(core::iso),"renew":sub.and_then(|s|s.get("renew")),"items":items,"balance":balance,"reason":if value.is_none(){Some("Command Code could not report this account's credit balance.")}else{None}})
}
#[cfg(test)]
#[path = "../test/readings.test.rs"]
mod tests;
