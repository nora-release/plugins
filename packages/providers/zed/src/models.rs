use super::{account, network, request::Wire};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::BTreeSet;
#[derive(Clone)]
pub(crate) struct Model {
    pub id: String,
    pub vendor: String,
    pub value: Value,
}
pub(crate) fn wire(vendor: &str) -> Option<Wire> {
    match vendor {
        "anthropic" => Some(Wire::Anthropic),
        "open_ai" => Some(Wire::Responses),
        "google" => Some(Wire::Gemini),
        "x_ai" => Some(Wire::Chat),
        _ => None,
    }
}
pub(crate) fn parse(value: &Value) -> PluginResult<Vec<Model>> {
    let rows = value["models"]
        .as_array()
        .filter(|r| r.len() <= 4096)
        .ok_or_else(|| PluginError::transport("Zed returned an invalid model catalog."))?;
    let mut seen = BTreeSet::new();
    let mut models = Vec::new();
    for row in rows {
        let Some(id) =
            account::text(row.get("id"), 512).filter(|s| !s.chars().any(char::is_whitespace))
        else {
            continue;
        };
        let Some(vendor) = row["provider"].as_str() else {
            continue;
        };
        let Some(wire) = wire(vendor) else {
            continue;
        };
        if row["is_disabled"] == true || !seen.insert(id) {
            continue;
        }
        let efforts: Vec<_> = row["supported_effort_levels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| account::text(e.get("value"), 64))
            .collect();
        let image = row["supports_images"] == true;
        let value = json!({"id":id,"name":account::text(row.get("display_name"),512).unwrap_or(id),"provider":"zed","api":wire.api(),"baseUrl":network::BASE,"contextWindow":row["max_token_count"].as_u64().unwrap_or(0),"maxTokens":row["max_output_tokens"].as_u64().unwrap_or(0),"reasoning":row["supports_thinking"]==true||!efforts.is_empty(),"input":if image{vec!["text","image"]}else{vec!["text"]},"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"info":{"toolCalling":true,"reasoningEfforts":efforts,"free":false}});
        models.push(Model {
            id: id.into(),
            vendor: vendor.into(),
            value,
        });
    }
    if models.is_empty() {
        return Err(PluginError::new(
            "zed_no_models",
            "Zed lists no callable models for this organization.",
        ));
    }
    Ok(models)
}
pub(crate) fn output(models: &[Model]) -> Value {
    json!({"models":models.iter().map(|m|m.value.clone()).collect::<Vec<_>>(),"source":"remote","allowUnknownModels":false,"defaultAPI":"anthropic-messages","defaultBaseURL":network::BASE})
}
#[cfg(test)]
#[path = "../test/models.test.rs"]
mod tests;
