use nora_provider_plugin_sdk::{PluginError, PluginResult, RequestConstraints};
use serde_json::Value;

pub(crate) fn gemini_thinking(body: Option<&[u8]>) -> Option<Value> {
    let value: Value = serde_json::from_slice(body?).ok()?;
    value.pointer("/generationConfig/thinkingConfig").cloned()
}

pub(crate) fn apply(
    api: &str,
    constraints: &RequestConstraints,
    gemini_thinking: Option<&Value>,
    body: &mut Option<Vec<u8>>,
) -> PluginResult<()> {
    let limit = constraints.output_token_limit.filter(|limit| *limit > 0);
    if limit.is_none() && !constraints.reasoning_disabled {
        return Ok(());
    }
    let Some(bytes) = body else { return Ok(()) };
    let mut value: Value = serde_json::from_slice(bytes)
        .map_err(|_| PluginError::invalid("The constrained provider request must contain JSON."))?;
    let fields = value.as_object_mut().ok_or_else(|| {
        PluginError::invalid("The constrained provider request must be an object.")
    })?;
    if let Some(limit) = limit {
        nora_ai::enforce_output_token_limit(api, fields, limit);
    }
    if constraints.reasoning_disabled {
        nora_ai::enforce_reasoning_disabled(api, fields, gemini_thinking);
    }
    *bytes = serde_json::to_vec(&value).map_err(|_| {
        PluginError::invalid("The constrained provider request could not be encoded.")
    })?;
    Ok(())
}
