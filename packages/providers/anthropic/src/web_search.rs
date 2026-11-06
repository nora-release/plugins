use nora_provider_plugin_sdk::PluginResult;
use serde_json::{json, Value};

pub(crate) fn is_search(tool: &Value) -> bool {
    tool["type"]
        .as_str()
        .is_some_and(|kind| kind.starts_with("web_search"))
        && tool.get("input_schema").is_none()
}

/// Subscription requests offer a search capability, as in Magpie. The installed
/// CLI owns its HTTP search version and options; this adapter never rewrites them.
pub(crate) fn requested(request: &Value) -> PluginResult<bool> {
    Ok(request["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .any(is_search))
}

#[cfg(test)]
use super::cli_messages::Receiver as SearchStream;

pub(crate) fn sum_usage(before: &Value, current: &Value) -> Value {
    let mut combined = before.as_object().cloned().unwrap_or_default();
    for (key, value) in current.as_object().into_iter().flatten() {
        let previous = combined.get(key).unwrap_or(&Value::Null);
        combined.insert(
            key.clone(),
            if let (Some(a), Some(b)) = (previous.as_u64(), value.as_u64()) {
                json!(a.saturating_add(b))
            } else if value.is_object() {
                sum_usage(previous, value)
            } else {
                value.clone()
            },
        );
    }
    Value::Object(combined)
}

#[cfg(test)]
#[path = "../test/web_search.test.rs"]
mod tests;
