use crate::{config::Account, pricing};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::BTreeSet;
fn text<'a>(entry: &'a Value, key: &str) -> Option<&'a str> {
    entry.get(key).and_then(Value::as_str).filter(|value| {
        !value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control)
    })
}
fn positive(entry: &Value, keys: &[&str]) -> u64 {
    keys.iter()
        .filter_map(|key| entry.get(*key))
        .filter_map(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
        .find(|value| *value > 0 && *value <= u32::MAX as u64)
        .unwrap_or(0)
}
fn labels(entry: &Value, key: &str, field: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    entry
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(64)
        .filter_map(|value| value.as_str().or_else(|| value.get(field)?.as_str()))
        .filter(|value| {
            !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
        })
        .filter(|value| seen.insert((*value).to_owned()))
        .map(str::to_owned)
        .collect()
}
fn protocol(entry: &Value) -> &'static str {
    for path in entry
        .get("native_endpoints")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        match path.trim_end_matches('/') {
            "/v1/messages" | "/messages" | "anthropic" | "messages" => return "anthropic-messages",
            "/v1/responses" | "/responses" | "responses" => return "openai-responses",
            "/v1/chat/completions" | "/chat/completions" | "chat" => return "openai-completions",
            "/v1/systemone" | "/systemone" | "decide" | "decision" => return "systemone",
            _ => {}
        }
    }
    if entry.get("kind").and_then(Value::as_str) == Some("decision") {
        return "systemone";
    }
    "openai-completions"
}
pub(crate) fn parse(payload: &Value, account: &Account) -> PluginResult<Value> {
    let data = payload
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| PluginError::transport("The remote gateway did not return a model list."))?;
    if data.len() > 4096 {
        return Err(PluginError::transport(
            "The remote gateway returned too many models.",
        ));
    }
    let mut ids = BTreeSet::new();
    let mut models = Vec::new();
    for entry in data {
        let Some(id) = text(entry, "id").filter(|id| id.len() <= 1024 && id.trim() == *id) else {
            continue;
        };
        if !ids.insert(id) {
            continue;
        }
        let kind = text(entry, "kind").unwrap_or("");
        let api = protocol(entry);
        let caps = &entry["capabilities"];
        let is_chat = !matches!(
            kind,
            "image" | "video" | "decision" | "embedding" | "rerank" | "audio"
        ) && caps.get("chat").and_then(Value::as_bool) != Some(false);
        let name = text(entry, "magpie_label")
            .or_else(|| text(entry, "display_name"))
            .or_else(|| text(entry, "name"))
            .unwrap_or(id);
        let modalities = entry
            .get("input_modalities")
            .or_else(|| entry.pointer("/modalities/input"))
            .and_then(Value::as_array);
        let mut input: Vec<Value> = modalities
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|value| matches!(*value, "text" | "image" | "audio" | "video" | "pdf"))
            .map(|value| json!(value))
            .collect();
        if input.is_empty() {
            input.push(json!("text"));
        }
        if caps.get("vision").and_then(Value::as_bool) == Some(true)
            && !input.contains(&json!("image"))
        {
            input.push(json!("image"));
        }
        let mut model = json!({"id":id,"name":name,"provider":"remote-magpie","api":api,"baseURL":if api=="anthropic-messages"{account.root.clone()}else{account.at("/v1")},"reasoning":entry.get("reasoning").and_then(Value::as_bool).or_else(||caps.get("reasoning").and_then(Value::as_bool)).unwrap_or(false),"input":input,"contextWindow":positive(entry,&["context_window","context_length","max_input_tokens"]),"maxTokens":positive(entry,&["max_output_tokens"]),"info":{"chat":is_chat,"imageOutput":kind=="image"||caps.get("image_output").and_then(Value::as_bool)==Some(true),"videoOutput":kind=="video"||caps.get("video_output").and_then(Value::as_bool)==Some(true),"embedding":kind=="embedding"||caps.get("embedding").and_then(Value::as_bool)==Some(true),"decision":kind=="decision"||api=="systemone"||caps.get("decision").and_then(Value::as_bool)==Some(true),"reasoningEfforts":labels(entry,"supported_reasoning_levels","effort"),"serviceTiers":labels(entry,"service_tiers","id")}});
        if !kind.is_empty() && kind.len() <= 64 {
            model["info"]["nativeKind"] = json!(kind);
        }
        if let Some(search) = entry
            .get("web_search")
            .and_then(Value::as_str)
            .filter(|search| matches!(*search, "native" | "magpie"))
        {
            model["info"]["remoteWebSearch"] = json!(if search == "native" {
                "native"
            } else {
                "proxy"
            });
            model["info"]["hostedToolCapabilities"] = json!({"hostedTools":["webSearch"]});
        }
        if let Some(tools) = caps
            .get("tools")
            .and_then(Value::as_bool)
            .or_else(|| entry.get("tool_calling").and_then(Value::as_bool))
        {
            model["info"]["toolCalling"] = json!(tools);
        }
        pricing::apply(&mut model, entry, id);
        models.push(model);
    }
    Ok(
        json!({"models":models,"source":"remote","allowUnknownModels":false,"defaultAPI":"openai-completions","defaultBaseURL":account.at("/v1")}),
    )
}
#[cfg(test)]
#[path = "../test/models.test.rs"]
mod tests;
