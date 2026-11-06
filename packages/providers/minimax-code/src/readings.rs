use crate::signing::Site;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use nora_provider_subscription_core as core;
use serde_json::{json, Value};
pub(crate) fn models(value: &Value, site: Site, source: &str) -> PluginResult<Vec<Value>> {
    let providers = value["providers"]
        .as_array()
        .ok_or_else(|| PluginError::transport("MiniMax Code returned no model configuration."))?;
    let config = &providers
        .iter()
        .find(|provider| provider["providerId"] == "minimax")
        .ok_or_else(|| PluginError::transport("MiniMax Code returned no MiniMax models."))?
        ["config"];
    let models = config["models"]
        .as_object()
        .ok_or_else(|| PluginError::transport("MiniMax Code returned an invalid model list."))?;
    if models.len() > core::MAX_MODELS {
        return Err(PluginError::transport(
            "MiniMax Code returned too many models.",
        ));
    }
    let mut ids: Vec<&str> = config["model_order"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|id| models.contains_key(*id))
        .collect();
    for id in models.keys() {
        if !ids.contains(&id.as_str()) {
            ids.push(id);
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for id in ids {
        if !seen.insert(id) {
            continue;
        }
        if id.trim().is_empty() || id.len() > 2048 || id.chars().any(char::is_control) {
            return Err(PluginError::transport(
                "MiniMax Code returned an invalid model ID.",
            ));
        }
        let entry = &models[id];
        let mode = entry
            .pointer("/thinking_config/mode")
            .and_then(Value::as_str);
        let reasoning = mode != Some("forced_off") && entry["reasoning"] != false;
        let images = entry["attachment"] == true
            || entry
                .pointer("/modalities/input")
                .and_then(Value::as_array)
                .is_some_and(|list| list.iter().any(|v| v == "image"));
        let mut efforts: Vec<String> = entry
            .pointer("/thinking/effortOptions")
            .or_else(|| entry.pointer("/thinking/effort_options"))
            .or_else(|| entry.get("effort_options"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|e| !e.is_empty() && *e != "default" && *e != "none")
            .map(str::to_owned)
            .collect();
        if mode == Some("switchable") {
            efforts.insert(0, "off".into());
            if efforts.len() == 1 {
                efforts.push("high".into());
            }
        }
        out.push(core::model(
            id,
            core::text(&entry["name"]).unwrap_or(id),
            "anthropic-messages",
            site.id,
            &site.base(),
            entry
                .pointer("/limit/context")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            entry
                .pointer("/limit/output")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            images,
            reasoning,
            efforts,
            source,
        ));
    }
    Ok(out)
}
pub(crate) fn presets(site: Site) -> Vec<Value> {
    let values: Vec<Value> = serde_json::from_str(include_str!("models.json")).unwrap_or_default();
    values
        .into_iter()
        .filter_map(|m| {
            Some(core::model(
                m["id"].as_str()?,
                m["name"].as_str()?,
                "anthropic-messages",
                site.id,
                &site.base(),
                m["context"].as_u64()?,
                m["output"].as_u64()?,
                m["images"] == true,
                m["thinking"] != "off",
                m["efforts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect(),
                "preset",
            ))
        })
        .collect()
}
pub(crate) fn membership(value: &Value) -> Value {
    let data = value.get("data").unwrap_or(value);
    let get = |key: &str| {
        value
            .get(key)
            .or_else(|| data.get(key))
            .cloned()
            .unwrap_or(Value::Null)
    };
    let balance = value
        .pointer("/op_credit_summary/total_remaining_amount")
        .or_else(|| data.pointer("/op_credit_summary/total_remaining_amount"))
        .cloned()
        .unwrap_or_else(|| get("opcredit_balance"));
    json!({"hasTokenPlan":get("has_token_plan"),"opGroupId":get("op_group_id"),"tier":get("token_plan_tier"),"expiresAt":get("token_plan_expires_at"),"balance":balance})
}
pub(crate) fn plan_windows(value: &Value) -> PluginResult<Vec<Value>> {
    if value
        .pointer("/base_resp/status_code")
        .and_then(Value::as_i64)
        .is_some_and(|n| n != 0)
    {
        return Err(PluginError::transport(
            "MiniMax refused the plan allowance request.",
        ));
    }
    let rows = value["model_remains"]
        .as_array()
        .ok_or_else(|| PluginError::transport("MiniMax returned no plan allowance."))?;
    let number = |value: &Value| {
        value
            .as_f64()
            .or_else(|| value.as_str().and_then(|v| v.parse::<f64>().ok()))
            .filter(|v| v.is_finite())
    };
    let mut items = Vec::new();
    for row in rows {
        let name = core::text(&row["model_name"]).unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let general = name.eq_ignore_ascii_case("general");
        for week in [false, true] {
            let (remaining, status, start, end) = if week {
                (
                    "current_weekly_remaining_percent",
                    "current_weekly_status",
                    "weekly_start_time",
                    "weekly_end_time",
                )
            } else {
                (
                    "current_interval_remaining_percent",
                    "current_interval_status",
                    "start_time",
                    "end_time",
                )
            };
            let state = number(&row[status]).unwrap_or(0.0);
            let left = number(&row[remaining]);
            if state == 3.0 || (left.is_none() && state != 2.0) {
                continue;
            }
            let used = if state == 2.0 {
                100.0
            } else {
                (100.0 - left.unwrap()).clamp(0.0, 100.0)
            };
            let at = core::timestamp(&row[end]);
            let from = core::timestamp(&row[start]);
            let span = at
                .zip(from)
                .filter(|(a, b)| a > b)
                .map(|(a, b)| (a - b) as f64 / 1000.0)
                .or(if week { Some(604800.0) } else { None });
            let label = if week {
                "7 days".to_owned()
            } else {
                span.map(|seconds| format!("{} hours", seconds / 3600.0))
                    .unwrap_or_else(|| "Allowance".into())
            };
            let mut item = core::usage_item(
                &if general {
                    label
                } else {
                    format!("{name} · {label}")
                },
                used,
                100.0,
                at,
                span,
                general,
            );
            item["aside"] = json!(!general);
            items.push(item);
        }
    }
    Ok(items)
}
#[cfg(test)]
#[path = "../test/readings.test.rs"]
mod tests;
