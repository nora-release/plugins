use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub(crate) fn parse(body: &Value, supplemental: Vec<Value>) -> PluginResult<Vec<Value>> {
    let entries = body["models"]
        .as_array()
        .filter(|items| items.len() <= 4096)
        .ok_or_else(|| PluginError::transport("ChatGPT returned an invalid model list."))?;
    let mut hidden = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for entry in entries {
        let Some(id) = entry["slug"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control))
        else {
            continue;
        };
        if entry["visibility"]
            .as_str()
            .is_some_and(|v| !v.is_empty() && v != "list")
        {
            hidden.insert(id.to_owned());
            continue;
        }
        if !seen.insert(id.to_owned()) {
            continue;
        }
        let name = entry["display_name"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(id);
        let context = entry["context_window"].as_u64().unwrap_or(0);
        let max = entry["max_context_window"].as_u64().unwrap_or(0);
        let efforts = entry["supported_reasoning_levels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|level| level["effort"].as_str())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let mut model = json!({"id":id,"name":name,"provider":"chatgpt-api","api":"openai-responses","baseURL":"https://api.openai.com/v1","contextWindow":context,"maxTokens":0,"reasoning":!efforts.is_empty(),"toolCalling":true,"info":{"chat":true,"reasoningEfforts":efforts}});
        if let Some(input) = entry["input_modalities"].as_array() {
            model["input"] = json!(if input.iter().any(|v| v == "image") {
                vec!["text", "image"]
            } else {
                vec!["text"]
            });
        }
        if max > context {
            model["info"]["maxContextWindow"] = json!(max);
        }
        out.push(model);
    }
    for mut model in supplemental {
        let Some(id) = model["id"]
            .as_str()
            .filter(|id| {
                !id.is_empty() && !id.contains('/') && !hidden.contains(*id) && !seen.contains(*id)
            })
            .map(str::to_owned)
        else {
            continue;
        };
        seen.insert(id);
        model["provider"] = json!("chatgpt-api");
        model["api"] = json!("openai-responses");
        model["baseURL"] = json!("https://api.openai.com/v1");
        out.push(model);
    }
    if out.is_empty() {
        return Err(PluginError::transport(
            "ChatGPT lists no models for this account.",
        ));
    }
    Ok(out)
}
pub(crate) fn codex_cache() -> Vec<Value> {
    let directory = std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|p| p.join(".codex"))
        });
    let Some(path) = directory.map(|p| p.join("models_cache.json")) else {
        return Vec::new();
    };
    if !std::fs::metadata(&path).is_ok_and(|m| m.len() <= 8 * 1024 * 1024) {
        return Vec::new();
    }
    let Some(payload) = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    else {
        return Vec::new();
    };
    let Some(entries) = payload["models"].as_array() else {
        return Vec::new();
    };
    parse(&json!({"models":entries}), Vec::new()).unwrap_or_default()
}

#[cfg(test)]
#[path = "../test/models.test.rs"]
mod tests;
