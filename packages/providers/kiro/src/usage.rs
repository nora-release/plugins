use super::session;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
pub(crate) fn plan(title: &str) -> String {
    title
        .split_whitespace()
        .map(|word| {
            let word = word.to_ascii_lowercase();
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
fn number(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0)
}
fn reset(value: Option<&Value>) -> Option<String> {
    let at = session::timestamp(value);
    chrono::DateTime::from_timestamp_millis(at)
        .filter(|_| at > 0)
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}
pub(crate) fn parse(value: &Value) -> PluginResult<Value> {
    let breakdown = value["usageBreakdownList"]
        .as_array()
        .ok_or_else(|| PluginError::transport("Kiro returned no credit allowances."))?;
    let mut items = Vec::new();
    let (mut total, mut used) = (0.0, 0.0);
    let mut next_reset: Option<String> = None;
    for (i, entry) in breakdown.iter().enumerate() {
        for (trial, quota) in [(false, entry), (true, &entry["freeTrialInfo"])] {
            if trial
                && !quota["freeTrialStatus"]
                    .as_str()
                    .is_some_and(|status| status.eq_ignore_ascii_case("ACTIVE"))
            {
                continue;
            }
            let Some(limit) =
                number(quota.get("usageLimitWithPrecision")).filter(|limit| *limit > 0.0)
            else {
                continue;
            };
            let Some(current) = number(quota.get("currentUsageWithPrecision")) else {
                continue;
            };
            let at = if trial {
                reset(quota.get("freeTrialExpiry"))
            } else {
                reset(quota.get("nextDateReset")).or_else(|| reset(value.get("nextDateReset")))
            };
            let mut item = json!({"id":format!("{}-{i}",if trial{"trial"}else{"credits"}),"label":if trial{"Free trial"}else{session::text(entry.get("displayNamePlural").or_else(||entry.get("displayName")),256).unwrap_or("Credits")},"type":"quota","unit":"credits","used":current,"limit":limit,"remaining":(limit-current).max(0.0),"usedPercent":100.0*current/limit,"countsForRouting":false});
            if let Some(at) = at {
                item["resetsAt"] = json!(at);
                if next_reset.as_ref().is_none_or(|previous| at < *previous) {
                    next_reset = Some(at);
                }
            }
            if !trial {
                item["windowSeconds"] = json!(30 * 24 * 3600);
            }
            // Trial and monthly credits are additive pools. Exhausting one must not disable another.
            total += limit;
            used += current.min(limit);
            items.push(item);
        }
    }
    if total > 0.0 {
        let mut combined = json!({"id":"chat-credits","label":"Chat credits","type":"quota","unit":"credits","used":used,"limit":total,"remaining":(total-used).max(0.0),"usedPercent":100.0*used/total,"countsForRouting":true});
        if let Some(at) = next_reset {
            combined["resetsAt"] = json!(at);
        }
        items.push(combined);
    }
    for item in &mut items {
        item["value"] = json!(format!("{} / {} credits", item["used"], item["limit"]));
    }
    let account = json!({"account":{"email":session::text(value.pointer("/userInfo/email"),512).unwrap_or_default(),"plan":plan(session::text(value.pointer("/subscriptionInfo/subscriptionTitle"),128).unwrap_or_default())}});
    Ok(nora_provider::usage::parsing::result(
        &account,
        items,
        Vec::new(),
    ))
}
pub(crate) fn models(value: &Value) -> PluginResult<Value> {
    let mut models = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for model in value["models"].as_array().into_iter().flatten() {
        let Some(id) = session::text(model.get("modelId"), 256).filter(|id| {
            id.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-:".contains(&b))
        }) else {
            continue;
        };
        if !seen.insert(id) {
            continue;
        }
        let images = model["supportedInputTypes"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|kind| {
                kind.as_str()
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("IMAGE"))
            });
        let thinking = id == "auto" || id.to_ascii_lowercase().contains("claude");
        let mut item = json!({"id":id,"name":session::text(model.get("modelName"),256).unwrap_or(if id=="auto"{"Auto"}else{id}),"provider":"kiro","api":"anthropic-messages","baseUrl":session::BASE,"reasoning":thinking,"input":if images{json!(["text","image"])}else{json!(["text"])},"contextWindow":model.pointer("/tokenLimits/maxInputTokens").and_then(Value::as_u64).unwrap_or(0),"maxTokens":model.pointer("/tokenLimits/maxOutputTokens").and_then(Value::as_u64).unwrap_or(0),"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"info":{"toolCalling":true}});
        if thinking {
            item["info"]["reasoningEfforts"] =
                json!(["none", "low", "medium", "high", "xhigh", "max"]);
        }
        if value
            .pointer("/defaultModel/modelId")
            .and_then(Value::as_str)
            == Some(id)
        {
            models.insert(0, item);
        } else {
            models.push(item);
        }
    }
    if models.is_empty() {
        return Err(PluginError::transport(
            "Kiro listed no models for this account.",
        ));
    }
    Ok(
        json!({"models":models,"source":"remote","allowUnknownModels":true,"defaultAPI":"anthropic-messages","defaultBaseURL":session::BASE}),
    )
}
#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
