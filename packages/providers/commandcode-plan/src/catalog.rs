use nora_provider_plugin_sdk::{PluginError, PluginResult};
use nora_provider_subscription_core as core;
use serde_json::{json, Value};
pub(crate) const BASE: &str = "https://api.commandcode.ai/provider/v1";
pub(crate) fn table(go: bool) -> Vec<Value> {
    serde_json::from_str(if go {
        include_str!("go_models.json")
    } else {
        include_str!("models.json")
    })
    .unwrap()
}
fn in_set(name: &str, id: &str) -> bool {
    let rules: Value = serde_json::from_str(include_str!("plan_rules.json")).unwrap();
    rules[name]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == id)
}
pub(crate) fn refuses(plan: &str, id: &str) -> bool {
    match plan {
        "Go" => in_set("PREMIUM", id) || in_set("GO_BLOCKED", id),
        "GOAT" => in_set("PREMIUM", id),
        "Pro" => in_set("PRO_BLOCKED", id),
        "Provider" => id == "gpt-6.1-sol",
        _ => false,
    }
}
fn known(id: &str) -> Option<Value> {
    table(false)
        .into_iter()
        .chain(table(true))
        .find(|row| row["id"] == id)
}
pub(crate) fn efforts(id: &str) -> Vec<String> {
    table(true)
        .into_iter()
        .find(|row| row["id"] == id)
        .and_then(|row| row["efforts"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}
fn convert(row: &Value, go: bool, source: &str) -> PluginResult<Value> {
    let id = core::text(&row["id"])
        .or_else(|| core::text(&row["name"]))
        .ok_or_else(|| PluginError::transport("Command Code returned a model without an ID."))?;
    if id.len() > 2048 || id.chars().any(char::is_control) {
        return Err(PluginError::transport(
            "Command Code returned an invalid model ID.",
        ));
    }
    let preset = known(id).unwrap_or(Value::Null);
    let name = core::text(&row["display_name"])
        .or_else(|| core::text(&row["name"]).filter(|name| *name != id))
        .or_else(|| core::text(&preset["name"]))
        .unwrap_or(id);
    let images = row["modalities"]["input"]
        .as_array()
        .map(|list| list.iter().any(|value| value == "image"))
        .or_else(|| row["images"].as_bool())
        .or_else(|| preset["images"].as_bool())
        .unwrap_or(!go && (id.starts_with("claude-") || id.starts_with("anthropic/")));
    let context = row["context_length"]
        .as_u64()
        .filter(|n| *n > 0)
        .or_else(|| row["context"].as_u64())
        .or_else(|| preset["context"].as_u64())
        .unwrap_or(0);
    let mut api = if id.starts_with("claude-") || id.starts_with("anthropic/") {
        "anthropic-messages"
    } else {
        "openai-completions"
    };
    if go {
        api = "openai-completions";
    } else if let Some(eps) = row["supported_endpoints"]
        .as_array()
        .filter(|eps| !eps.is_empty())
    {
        let endpoints: Vec<String> = eps
            .iter()
            .filter_map(Value::as_str)
            .map(|ep| {
                ep.trim_end_matches('/')
                    .strip_prefix("/v1")
                    .unwrap_or(ep.trim_end_matches('/'))
                    .to_owned()
            })
            .collect();
        api = if endpoints.iter().any(|s| s == "/chat/completions") {
            "openai-completions"
        } else if endpoints.iter().any(|s| s == "/responses") {
            "openai-responses"
        } else if endpoints.iter().any(|s| s == "/messages") {
            "anthropic-messages"
        } else {
            "openai-completions"
        };
    }
    let levels = if go {
        efforts(id)
    } else {
        row["efforts"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    let output = row["output"]
        .as_u64()
        .unwrap_or(if go { 200_000 } else { 0 });
    let mut model = core::model(
        id,
        name,
        api,
        "commandcode-plan",
        BASE,
        context,
        output,
        images,
        !levels.is_empty(),
        levels,
        source,
    );
    model["info"]["free"] = json!(in_set("FREE", id));
    Ok(model)
}
pub(crate) fn presets(plan: &str, extra: bool) -> Vec<Value> {
    table(plan == "Go")
        .iter()
        .filter(|row| extra || !refuses(plan, row["id"].as_str().unwrap()))
        .filter_map(|row| convert(row, plan == "Go", "preset").ok())
        .collect()
}
pub(crate) fn remote(value: &Value, plan: &str, extra: bool) -> PluginResult<Vec<Value>> {
    let rows = value["data"]
        .as_array()
        .filter(|rows| !rows.is_empty())
        .or_else(|| value["models"].as_array())
        .ok_or_else(|| PluginError::transport("Command Code returned no model list."))?;
    if rows.len() > core::MAX_MODELS {
        return Err(PluginError::transport(
            "Command Code returned too many models.",
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for row in rows {
        if plan == "Go" {
            let id = core::text(&row["id"])
                .or_else(|| core::text(&row["name"]))
                .unwrap_or("");
            let lower = id.to_lowercase();
            if [
                "image",
                "imagen",
                "imagine",
                "dall-e",
                "flux",
                "seedream",
                "cogview",
                "stable-diffusion",
                "sdxl",
                "wanx",
                "kolors",
                "hidream",
            ]
            .iter()
            .any(|part| lower.contains(part))
                || [
                    "embed",
                    "-tts",
                    "audio",
                    "-live",
                    "robotics",
                    "computer-use",
                    "deep-research",
                    "transcribe",
                    "realtime",
                    "moderation",
                    "whisper",
                    "sora",
                ]
                .iter()
                .any(|part| id.contains(part))
            {
                continue;
            }
        }
        let model = convert(row, plan == "Go", "remote")?;
        let id = model["id"].as_str().unwrap();
        if (extra || !refuses(plan, id)) && seen.insert(id.to_owned()) {
            out.push(model);
        }
    }
    if out.is_empty() {
        return Err(PluginError::transport(
            "Command Code listed no models for this plan.",
        ));
    }
    Ok(out)
}
#[cfg(test)]
#[path = "../test/catalog.test.rs"]
mod tests;
