use crate::{
    auth,
    endpoint::{self, Site},
};
use nora_ai::{HttpClient, HttpMethod, HttpRequest};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use nora_provider_subscription_core as core;
use serde_json::{json, Value};
use std::collections::BTreeSet;
pub(crate) fn presets(start: bool) -> Vec<Value> {
    serde_json::from_str::<Vec<Value>>(include_str!("models.json"))
        .unwrap()
        .into_iter()
        .filter(|row| !start || row["id"] != "GLM-5.3")
        .collect()
}
pub(crate) fn rules(value: &Value, site: Site, start: bool) -> Vec<Value> {
    let plan = if start {
        "account:zai-start-plan"
    } else if site == Site::Zai {
        "account:zai-individual-coding-plan"
    } else {
        "account:bigmodel-individual-coding-plan"
    };
    let config = &value["config"];
    let mut ids = Vec::new();
    let mut seen = BTreeSet::new();
    let mut off = BTreeSet::new();
    for rule in config["providerConfigRules"]["providerRules"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|rule| rule["providerId"] == plan)
    {
        for id in rule["builtinModelIds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if seen.insert(id.to_lowercase()) {
                ids.push(id.to_owned());
            }
        }
    }
    for rule in config["modelConfigRules"]["builtinProviderModelRules"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|rule| rule["providerId"] == plan)
    {
        let Some(id) = core::text(&rule["modelId"]) else {
            continue;
        };
        if rule["config"]["enabled"] == false {
            off.insert(id.to_lowercase());
        } else if seen.insert(id.to_lowercase()) {
            ids.push(id.to_owned());
        }
    }
    let mut out = Vec::new();
    for id in ids {
        if off.contains(&id.to_lowercase()) {
            continue;
        }
        let mut model = json!({"id":id,"context":0,"output":0,"efforts":[]});
        for rule in config["modelConfigRules"]["modelRules"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let Some(pattern) = rule["modelMatch"].as_str() else {
                continue;
            };
            let Ok(pattern) = regex::Regex::new(&format!("(?i)^(?:{pattern})$")) else {
                continue;
            };
            if !pattern.is_match(&id) {
                continue;
            }
            let properties = &rule["config"]["properties"];
            let specs = &rule["config"]["optionSpecs"];
            if properties["contextWindow"].as_u64().is_some_and(|n| n > 0) {
                model["context"] = properties["contextWindow"].clone();
            }
            if specs["maxOutputTokens"]["max"]
                .as_u64()
                .is_some_and(|n| n > 0)
            {
                model["output"] = specs["maxOutputTokens"]["max"].clone();
            }
            if properties["inputFormat"]["supportsImage"].is_boolean() {
                model["image"] = properties["inputFormat"]["supportsImage"].clone();
            }
            if let Some(values) = specs["reasoningLevel"]["values"]
                .as_array()
                .filter(|values| !values.is_empty())
            {
                let mut seen = BTreeSet::new();
                model["efforts"] = json!(values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|value| match value {
                        "disabled" => "none",
                        "enabled" => "high",
                        _ => value,
                    })
                    .filter(|value| seen.insert(*value))
                    .collect::<Vec<_>>());
            }
        }
        out.push(model);
    }
    out
}
pub(crate) fn model(row: &Value, site: Site, start: bool, source: &str) -> Value {
    let id = row["id"].as_str().unwrap();
    let levels: Vec<String> = row["efforts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    core::model(
        id,
        id,
        "anthropic-messages",
        "zcode",
        &format!("{}/v1", if start { endpoint::START } else { site.base() }),
        row["context"].as_u64().unwrap_or(0),
        row["output"].as_u64().unwrap_or(0),
        row["image"] == true,
        levels.iter().any(|level| level != "none"),
        levels,
        source,
    )
}
pub(crate) fn about(id: &str) -> Value {
    presets(false)
        .into_iter()
        .find(|row| {
            row["id"]
                .as_str()
                .is_some_and(|name| name.eq_ignore_ascii_case(id))
        })
        .unwrap_or_else(|| json!({"id":id,"context":0,"output":0,"efforts":[]}))
}
pub(crate) async fn remote(client: &dyn HttpClient, device: &str) -> PluginResult<Value> {
    let data = endpoint::call(
        client,
        HttpMethod::Get,
        &format!(
            "{}/api/v1/client/configs?app_version={}&platform={}",
            endpoint::ZCODE,
            endpoint::VERSION,
            endpoint::platform()
        ),
        None,
        None,
        Default::default(),
        device,
    )
    .await?;
    let raw = core::required(&data["configs"], "builtin_provider_config_json")?;
    let url = url::Url::parse(raw)
        .map_err(|_| PluginError::transport("ZCode returned an invalid model config address."))?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(PluginError::transport(
            "ZCode's model config requires an HTTPS address without credentials.",
        ));
    }
    let response = client
        .stream(
            HttpRequest::new(HttpMethod::Get, raw)
                .with_header("user-agent", format!("ZCode/{}", endpoint::VERSION)),
        )
        .await
        .map_err(|_| PluginError::transport("ZCode's model config could not be reached."))?;
    if !(200..300).contains(&response.status) {
        return Err(PluginError::transport(format!(
            "ZCode's model config returned HTTP {}.",
            response.status
        )));
    }
    let bytes = core::collect(response, 16 * 1024 * 1024).await?;
    serde_json::from_slice(&bytes)
        .map_err(|_| PluginError::transport("ZCode returned an invalid model config."))
}
pub(crate) fn local(source: &Value) -> PluginResult<Option<Value>> {
    let Some(path) = core::text(&source["providerConfigPath"]) else {
        return Ok(None);
    };
    let (_, value) = auth::read_json(path)?;
    Ok(Some(value))
}
#[cfg(test)]
#[path = "../test/catalog.test.rs"]
mod tests;
