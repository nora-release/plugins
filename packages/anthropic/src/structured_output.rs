use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::Value;

pub(crate) const MISSING: &str =
    "Claude Code did not return an answer matching the requested JSON Schema.";
pub(crate) fn schema(request: &Value) -> PluginResult<Option<Value>> {
    let Some(format) = request
        .pointer("/output_config/format")
        .filter(|format| !format.is_null())
    else {
        return Ok(None);
    };
    if format["type"] != "json_schema" || !format["schema"].is_object() {
        return Err(PluginError::invalid(
            "Claude Code requires a JSON Schema object for structured output.",
        ));
    }
    Ok(Some(format["schema"].clone()))
}

#[cfg(test)]
#[path = "../test/structured_output.test.rs"]
mod tests;
