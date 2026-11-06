use nora_ai::{InputModality, Model};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub(super) fn parse(payload: &Value) -> PluginResult<Value> {
    let invalid = || PluginError::transport("The Kimi Work model catalog is invalid.");
    let rows = payload
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?;
    if rows.is_empty()
        || rows.len() > 100
        || payload
            .get("has_more")
            .is_some_and(|more| !more.is_null() && more != &json!(false))
    {
        return Err(invalid());
    }
    let mut models = Vec::new();
    let mut known = BTreeSet::new();
    for row in rows {
        let Some(id) = row.get("id").and_then(Value::as_str).filter(|id| {
            !id.is_empty()
                && id.len() <= 256
                && !id.chars().any(|character| {
                    character.is_control() || character.is_whitespace() || character == '/'
                })
        }) else {
            continue;
        };
        if !known.insert(id) {
            continue;
        }
        let mut model = Model::new(id, "openai-completions", "kimi-work");
        model.base_url = super::BASE.into();
        model.name = row
            .get("display_name")
            .and_then(Value::as_str)
            .filter(|name| {
                !name.trim().is_empty() && name.len() <= 512 && !name.chars().any(char::is_control)
            })
            .unwrap_or(id)
            .into();
        model.context_window = row
            .get("context_length")
            .and_then(Value::as_u64)
            .filter(|context| *context > 0)
            .unwrap_or(262_144)
            .min(262_144);
        model.max_tokens = 32_768;
        model.reasoning = row.get("supports_reasoning").and_then(Value::as_bool) == Some(true);
        model.info.tool_calling = Some(true);
        if row.get("supports_image_in").and_then(Value::as_bool) == Some(true) {
            model.input = vec![InputModality::Text, InputModality::Image];
        }
        if let Some(options) = row.get("reasoningEffortOptions") {
            let options = options
                .as_array()
                .filter(|options| options.len() <= 64)
                .ok_or_else(invalid)?;
            for option in options {
                let raw = option
                    .get("effort")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid)?;
                let effort = raw
                    .strip_prefix("REASONING_EFFORT_")
                    .unwrap_or(raw)
                    .to_ascii_lowercase();
                if effort.is_empty()
                    || effort.len() > 64
                    || !effort.as_bytes()[0].is_ascii_lowercase()
                    || !effort.bytes().all(|byte| {
                        byte.is_ascii_lowercase()
                            || byte.is_ascii_digit()
                            || matches!(byte, b'_' | b'-')
                    })
                {
                    return Err(invalid());
                }
                if !model.info.reasoning_efforts.contains(&effort) {
                    model.info.reasoning_efforts.push(effort);
                }
            }
        }
        models.push(model);
    }
    if models.is_empty() {
        return Err(invalid());
    }
    Ok(
        json!({"models":models,"source":"remote","allowUnknownModels":false,"defaultAPI":"openai-completions","defaultBaseURL":super::BASE}),
    )
}

#[cfg(test)]
#[path = "../test/models.test.rs"]
mod tests;
