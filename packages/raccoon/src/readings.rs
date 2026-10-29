use crate::auth::BASE;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use nora_provider_subscription_core as core;
use serde_json::{json, Value};
pub(crate) fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse::<f64>().ok()))
        .filter(|n| n.is_finite())
}
fn convert(entry: &Value, source: &str) -> PluginResult<Value> {
    let id = core::text(&entry["name"])
        .or_else(|| core::text(&entry["model_name"]))
        .ok_or_else(|| PluginError::transport("Raccoon returned a model with no ID."))?;
    if id.len() > 2048 || id.chars().any(char::is_control) {
        return Err(PluginError::transport(
            "Raccoon returned an invalid model ID.",
        ));
    }
    let name = core::text(&entry["display_name"])
        .or_else(|| core::text(&entry["description"]).filter(|s| s.chars().count() <= 64))
        .unwrap_or(id);
    let images = entry["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .any(|tag| {
            ["vision", "image", "image-understanding"].contains(&tag.to_ascii_lowercase().as_str())
        });
    let context = ["context_window", "contextWindow"]
        .iter()
        .find_map(|key| number(&entry[key]).or_else(|| number(&entry["params"][key])))
        .filter(|n| *n > 0.0)
        .unwrap_or(200_000.0) as u64;
    let output = ["max_tokens", "maxTokens"]
        .iter()
        .find_map(|key| number(&entry[key]).or_else(|| number(&entry["params"][key])))
        .filter(|n| *n > 0.0)
        .unwrap_or(64_000.0) as u64;
    let rate = number(&entry["billing_effective_multiplier"])
        .filter(|n| *n >= 0.0)
        .or_else(|| number(&entry["billing_multiplier"]).filter(|n| *n >= 0.0));
    let mut model = core::model(
        id,
        name,
        "openai-completions",
        "raccoon",
        BASE,
        context,
        output,
        images,
        true,
        Vec::new(),
        source,
    );
    model["info"]["free"] = json!(rate == Some(0.0));
    if let Some(rate) = rate {
        model["info"]["creditMultiplier"] = json!(rate);
        if let Some(was) = number(&entry["billing_multiplier"]).filter(|was| *was > rate) {
            model["info"]["previousCreditMultiplier"] = json!(was);
        }
    }
    Ok(model)
}
pub(crate) fn models(value: &Value) -> PluginResult<Vec<Value>> {
    let data = value
        .get("data")
        .filter(|value| value.is_object())
        .unwrap_or(value);
    let categories = data["categories"]
        .as_array()
        .ok_or_else(|| PluginError::transport("Raccoon returned no model catalog."))?;
    let mut seen = std::collections::BTreeSet::new();
    let mut output = Vec::new();
    for entry in categories
        .iter()
        .flat_map(|cat| cat["models"].as_array().into_iter().flatten())
    {
        let model = convert(entry, "remote")?;
        let id = model["id"].as_str().unwrap();
        if seen.insert(id.to_owned()) {
            output.push(model);
        }
        if output.len() > core::MAX_MODELS {
            return Err(PluginError::transport("Raccoon returned too many models."));
        }
    }
    if !seen.contains("raccoon-chat-ml-5-5") {
        let mut model = core::model(
            "raccoon-chat-ml-5-5",
            "Raccoon Chat (默认)",
            "openai-completions",
            "raccoon",
            BASE,
            180_000,
            80_000,
            false,
            true,
            Vec::new(),
            "preset",
        );
        model["info"]["creditMultiplier"] = json!(1);
        model["info"]["free"] = json!(false);
        output.push(model);
    }
    Ok(output)
}
pub(crate) fn presets() -> Vec<Value> {
    let rows: Vec<Value> = serde_json::from_str(include_str!("models.json")).unwrap_or_default();
    rows.into_iter()
        .filter_map(|row| {
            let mut model = core::model(
                row["id"].as_str()?,
                row["name"].as_str()?,
                "openai-completions",
                "raccoon",
                BASE,
                row["context"].as_u64()?,
                row["output"].as_u64()?,
                row["images"] == true,
                row["reasoning"] == true,
                Vec::new(),
                "preset",
            );
            model["info"]["free"] = row["free"].clone();
            model["info"]["creditMultiplier"] = row["rate"].clone();
            if row["rateWas"]
                .as_f64()
                .zip(row["rate"].as_f64())
                .is_some_and(|(before, current)| before > current)
            {
                model["info"]["previousCreditMultiplier"] = row["rateWas"].clone();
            }
            Some(model)
        })
        .collect()
}
pub(crate) fn usage(
    points: &Value,
    entitlement: &Value,
    bills: &Value,
    account: &str,
) -> PluginResult<Value> {
    let available = number(&points["available_points"])
        .filter(|n| *n >= 0.0)
        .ok_or_else(|| PluginError::transport("Raccoon returned no credit balance."))?
        .floor();
    let office = entitlement
        .get("office")
        .or_else(|| entitlement.get("code"))
        .unwrap_or(&Value::Null);
    let pro = office["pro_enable"] == true || entitlement["code"]["pro_enable"] == true;
    let plan = if pro {
        core::text(&office["active_plan"]["type"])
            .or_else(|| core::text(&office["plan_type"]))
            .filter(|p| *p != "none")
            .unwrap_or("Pro")
    } else {
        "Free"
    };
    let until = core::timestamp(&office["pro_expired_time"])
        .or_else(|| core::timestamp(&office["active_plan"]["subscription_expired_at"]));
    let mut granted = 0.0;
    let mut soon = None;
    let mut expiring = 0.0;
    for bill in bills["items"].as_array().into_iter().flatten() {
        let Some(points) = number(&bill["points"]).filter(|n| *n > 0.0) else {
            continue;
        };
        let points = points.floor();
        let expiry = core::timestamp(&bill["expire_at"]);
        if expiry.is_some_and(|at| at <= core::now()) {
            continue;
        }
        granted += points;
        if let Some(at) = expiry {
            if soon.is_none_or(|previous| at < previous) {
                soon = Some(at);
                expiring = points;
            } else if soon == Some(at) {
                expiring += points;
            }
        }
    }
    let mut items = Vec::new();
    let balance = if granted > 0.0 && granted >= available {
        let mut item = core::usage_item("Credits", granted - available, granted, None, None, true);
        item["runsOutAt"] = soon.map(core::iso).unwrap_or(Value::Null);
        item["runsOut"] = json!(expiring.min(available));
        items.push(item);
        None
    } else {
        Some(format!("{available} credits"))
    };
    Ok(
        json!({"available":true,"account":{"id":account,"plan":plan},"plan":plan,"until":if pro{until.map(core::iso)}else{None},"balance":balance,"items":items}),
    )
}
pub(crate) fn today() -> String {
    chrono::DateTime::from_timestamp_millis((core::now() + 8 * 3600_000) as i64)
        .map(|date| date.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}
pub(crate) fn bill_key(item: &Value) -> String {
    core::text(&item["bill_id"])
        .or_else(|| core::text(&item["biz_id"]))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            format!(
                "{}:{}:{}",
                item["created_at"], item["points"], item["event_name"]
            )
        })
}
pub(crate) fn grants(item: &Value, today: &str) -> bool {
    number(&item["points"]).is_some_and(|n| n > 0.0)
        && item["created_at"]
            .as_str()
            .is_some_and(|at| at.get(..10) == Some(today))
        && ["", "daily_grant", "reward_grant"].contains(&item["biz_type"].as_str().unwrap_or(""))
}
#[cfg(test)]
#[path = "../test/readings.test.rs"]
mod tests;
