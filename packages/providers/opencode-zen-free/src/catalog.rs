use crate::protocol::Protocol;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use nora_provider_subscription_core as core;
use serde_json::{json, Value};
use std::collections::BTreeMap;
pub(crate) const BASE: &str = "https://opencode.ai/inference/v1";
pub(crate) const LEGACY: &str = "https://opencode.ai/zen/v1";
pub(crate) const CATALOG: &str = "https://models.opencode.ai/api.json";
pub(crate) const DOCS:&str="https://raw.githubusercontent.com/anomalyco/opencode/dev/services/www/src/docs/content/console/models.mdx";
pub(crate) fn bootstrap() -> Value {
    serde_json::from_str(include_str!("bootstrap.json")).unwrap()
}
pub(crate) fn protocols() -> BTreeMap<String, String> {
    serde_json::from_str(include_str!("protocols.json")).unwrap()
}
pub(crate) fn parse_docs(text: &str) -> BTreeMap<String, String> {
    let pattern=regex::Regex::new(r"\|[^|]+\|\s*`?([^|`\s]+)`?\s*\|\s*`[^`]+/v1/(chat/completions|responses|messages|systemone)`").unwrap();
    pattern
        .captures_iter(text)
        .map(|capture| {
            (
                capture[1].to_owned(),
                match &capture[2] {
                    "chat/completions" => "chat",
                    "responses" => "responses",
                    "messages" => "anthropic",
                    _ => "systemone",
                }
                .to_owned(),
            )
        })
        .collect()
}
fn sdk(value: &str) -> Option<Protocol> {
    if value.contains("anthropic") {
        Some(Protocol::Anthropic)
    } else if value == "@ai-sdk/openai" {
        Some(Protocol::Responses)
    } else if value.contains("openai-compatible") {
        Some(Protocol::Chat)
    } else {
        None
    }
}
fn free(id: &str, raw: &Value) -> bool {
    raw["deprecated"] != true
        && ![Some("deprecated"), Some("retired"), Some("disabled")]
            .contains(&raw["status"].as_str().or_else(|| raw["lifecycle"].as_str()))
        && raw["deprecated_at"].is_null()
        && raw["retirement_date"].is_null()
        && (id.to_lowercase().contains("free")
            || (raw["cost"]["input"].as_f64() == Some(0.)
                && raw["cost"]["output"].as_f64() == Some(0.)))
}
pub(crate) fn build(
    ids: impl IntoIterator<Item = String>,
    provider: &Value,
    protocols: &BTreeMap<String, String>,
    source: &str,
) -> PluginResult<BTreeMap<String, Value>> {
    let mut out = BTreeMap::new();
    for id in ids {
        if id.is_empty() || id == "__proto__" || id.len() > 2048 || id.chars().any(char::is_control)
        {
            continue;
        }
        let raw = &provider["models"][&id];
        let protocol = protocols
            .get(&id)
            .and_then(|p| Protocol::parse(p))
            .or_else(|| {
                if protocols.contains_key(&id) {
                    None
                } else if raw.is_object() {
                    sdk(raw["provider"]["npm"]
                        .as_str()
                        .or_else(|| provider["npm"].as_str())
                        .unwrap_or(""))
                } else if id.ends_with("-free") {
                    sdk(provider["npm"].as_str().unwrap_or("")).or(Some(Protocol::Chat))
                } else {
                    None
                }
            });
        let Some(protocol) = protocol else {
            continue;
        };
        if !free(&id, raw) {
            continue;
        }
        let efforts = raw["reasoning_options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|row| row["type"] == "effort")
            .flat_map(|row| row["values"].as_array().into_iter().flatten())
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let images = raw["modalities"]["input"]
            .as_array()
            .is_some_and(|input| input.iter().any(|value| value == "image"));
        let mut model = core::model(
            &id,
            core::text(&raw["name"]).unwrap_or(&id),
            protocol.api(),
            "opencode-zen-free",
            BASE,
            raw["limit"]["context"].as_u64().unwrap_or(32768),
            raw["limit"]["output"].as_u64().unwrap_or(4096),
            images,
            raw["reasoning"] == true,
            efforts,
            source,
        );
        model["input"] = raw["modalities"]["input"]
            .as_array()
            .map(|input| {
                Value::Array(
                    input
                        .iter()
                        .filter(|value| {
                            ["text", "image", "audio", "video", "pdf"]
                                .iter()
                                .any(|modality| value.as_str() == Some(*modality))
                        })
                        .cloned()
                        .collect(),
                )
            })
            .unwrap_or_else(|| json!(["text"]));
        model["info"]["free"] = json!(true);
        model["info"]["pricingKnown"] = json!(true);
        model["info"]["toolCalling"] = json!(raw["tool_call"].as_bool().unwrap_or(true));
        model["info"]["temperature"] = json!(raw["temperature"].as_bool().unwrap_or(true));
        if !raw["release_date"].is_null() {
            model["info"]["releaseDate"] = raw["release_date"].clone();
        }
        out.insert(id, model);
        if out.len() > core::MAX_MODELS {
            return Err(PluginError::transport("Zen returned too many free models."));
        }
    }
    Ok(out)
}
pub(crate) fn presets() -> BTreeMap<String, Value> {
    let provider = bootstrap();
    build(
        provider["models"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        &provider,
        &protocols(),
        "preset",
    )
    .unwrap()
}
#[cfg(test)]
#[path = "../test/catalog.test.rs"]
mod tests;
