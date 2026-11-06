use chrono::TimeZone;
use nora_provider_subscription_core as core;
use serde_json::{json, Value};
pub(crate) fn server_time(value: &Value) -> Option<u64> {
    let text = value.as_str()?.trim();
    if let Ok(value) = chrono::DateTime::parse_from_rfc3339(text) {
        return u64::try_from(value.timestamp_millis()).ok();
    }
    let text = text.replace(' ', "T");
    let naive = chrono::NaiveDateTime::parse_from_str(&text, "%Y-%m-%dT%H:%M:%S%.f")
        .ok()
        .or_else(|| {
            chrono::NaiveDate::parse_from_str(&text, "%Y-%m-%d")
                .ok()
                .and_then(|date| date.and_hms_opt(0, 0, 0))
        })?;
    u64::try_from(
        chrono::FixedOffset::east_opt(8 * 3600)?
            .from_local_datetime(&naive)
            .single()?
            .timestamp_millis(),
    )
    .ok()
}
pub(crate) fn credits(value: f64) -> String {
    for (divisor, suffix) in [(1e9, "B"), (1e6, "M"), (1e3, "K")] {
        if value >= divisor {
            let number = value / divisor;
            let decimals = (2. - number.log10().floor()).max(0.) as usize;
            let mut text = format!("{number:.decimals$}");
            if text.contains('.') {
                text = text.trim_end_matches('0').trim_end_matches('.').to_owned();
            }
            return format!("{text}{suffix}");
        }
    }
    value.to_string()
}
pub(crate) fn app_plan(subscription: &Value) -> Option<Value> {
    let current = subscription
        .get("current")
        .filter(|current| current.is_object())?;
    let tier = match current["planTier"].as_u64() {
        Some(1) => "Starter",
        Some(2) => "Plus",
        Some(3) => "Pro",
        Some(4) => "Ultra",
        _ => "",
    };
    let name = core::text(&current["title"])
        .or_else(|| if tier.is_empty() { None } else { Some(tier) })
        .or_else(|| core::text(&current["planCode"]))
        .unwrap_or("MiMo");
    Some(
        json!({"plan":name,"until":server_time(&current["endTime"]).map(core::iso),"renew":match current["renewalMode"].as_str(){Some("MONTHLY"|"YEARLY")=>Some("auto"),Some("ONE_TIME")=>Some("off"),_=>None}}),
    )
}
pub(crate) fn token_plan(detail: &Value, usage: Option<&Value>, app: bool) -> Option<Value> {
    let name = core::text(&detail["planName"])?;
    if detail["expired"] != false {
        return None;
    }
    let until = detail["currentPeriodEnd"]
        .as_str()
        .filter(|text| text.len() == 19)
        .and_then(|text| chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").ok())
        .and_then(|date| u64::try_from(date.and_utc().timestamp_millis()).ok());
    let mut value = json!({"plan":format!("Token Plan {name}"),"until":until.map(core::iso),"renew":if detail["enableAutoRenew"]==true||detail["hasAutoRenewSubscribed"]==true{"auto"}else{"off"}});
    if let Some(usage) = usage {
        let entries = usage["monthUsage"]["items"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(usage["usage"]["items"].as_array().into_iter().flatten())
            .collect::<Vec<_>>();
        if let Some(item) = entries
            .iter()
            .find(|entry| entry["name"] == "month_total_token")
            .or_else(|| {
                entries
                    .iter()
                    .find(|entry| entry["name"] == "plan_total_token")
            })
        {
            let limit = item["limit"]
                .as_f64()
                .or_else(|| {
                    item["limit"]
                        .as_str()
                        .and_then(|value| value.parse::<f64>().ok())
                })
                .filter(|limit| limit.is_finite() && *limit > 0.);
            if let Some((used, limit)) = item["used"]
                .as_f64()
                .filter(|used| used.is_finite())
                .zip(limit)
            {
                let mut window = core::usage_item(
                    if app {
                        "Token Plan · API key"
                    } else {
                        "Token Plan"
                    },
                    used.max(0.),
                    limit,
                    until,
                    None,
                    !app,
                );
                window["usedPercent"] = json!((used / limit * 100.).clamp(0., 100.));
                window["display"] =
                    json!(format!("{} / {} credits", credits(used), credits(limit)));
                value["item"] = window;
            }
        }
    }
    Some(value)
}
pub(crate) fn usage(
    subscription: &Value,
    usage: Option<&Value>,
    token_plan: Option<Value>,
    account: &str,
) -> Value {
    let app = app_plan(subscription);
    let mut value = app
        .clone()
        .or_else(|| {
            token_plan.as_ref().map(|plan| {
                let mut plan = plan.clone();
                plan.as_object_mut().unwrap().remove("item");
                plan
            })
        })
        .unwrap_or_else(|| json!({"plan":"Free"}));
    value["available"] = json!(true);
    value["account"] = json!({"id":account});
    let mut items = Vec::new();
    if let Some(usage) = usage {
        if let Some(percent) = usage["percent"]
            .as_f64()
            .filter(|percent| percent.is_finite() && usage["resetDate"].is_string())
        {
            items.push(core::usage_item(
                "7 days",
                (100. - percent).clamp(0., 100.),
                100.,
                server_time(&usage["resetDate"]),
                Some(7. * 24. * 3600.),
                true,
            ));
        }
    }
    if let Some(item) = token_plan.and_then(|plan| plan.get("item").cloned()) {
        items.push(item);
    }
    value["items"] = json!(items);
    value
}
#[cfg(test)]
#[path = "../test/readings.test.rs"]
mod tests;
