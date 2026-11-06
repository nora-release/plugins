use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::BTreeSet;
fn invalid() -> PluginError {
    PluginError::invalid(
        "Factory cannot wrap a recursive tool schema without its own $id resource.",
    )
}
fn rebase(schema: &mut Value, mut enabled: bool, legacy: bool, depth: usize) -> PluginResult<()> {
    if depth > 64 {
        return Err(PluginError::invalid(
            "The Factory tool schema is too deeply nested.",
        ));
    }
    let Some(obj) = schema.as_object_mut() else {
        return Ok(());
    };
    let id = obj
        .get("$id")
        .or_else(|| if legacy { obj.get("id") } else { None })
        .and_then(Value::as_str);
    if id.is_some_and(|id| !id.split('#').next().unwrap_or_default().is_empty()) {
        enabled = false;
    }
    if enabled {
        if obj.get("$recursiveAnchor") == Some(&json!(true)) || obj.contains_key("$recursiveRef") {
            return Err(invalid());
        }
        for key in ["$ref", "$dynamicRef"] {
            if let Some(reference) = obj
                .get(key)
                .and_then(Value::as_str)
                .filter(|s| s.starts_with('#'))
            {
                let fragment = percent_encoding::percent_decode_str(&reference[1..])
                    .decode_utf8()
                    .map_err(|_| {
                        PluginError::invalid("The tool schema has an invalid local reference.")
                    })?;
                if fragment.is_empty() || fragment.starts_with('/') {
                    let next = format!("#/properties/arguments{}", &reference[1..]);
                    obj.insert(key.into(), json!(next));
                }
            }
        }
    }
    for key in [
        "properties",
        "patternProperties",
        "$defs",
        "definitions",
        "dependentSchemas",
        "dependencies",
    ] {
        if let Some(children) = obj.get_mut(key).and_then(Value::as_object_mut) {
            for child in children.values_mut() {
                rebase(child, enabled, legacy, depth + 1)?;
            }
        }
    }
    for key in ["anyOf", "oneOf", "allOf", "prefixItems"] {
        if let Some(children) = obj.get_mut(key).and_then(Value::as_array_mut) {
            for child in children {
                rebase(child, enabled, legacy, depth + 1)?;
            }
        }
    }
    for key in [
        "additionalProperties",
        "unevaluatedProperties",
        "propertyNames",
        "contains",
        "items",
        "additionalItems",
        "unevaluatedItems",
        "not",
        "if",
        "then",
        "else",
        "contentSchema",
    ] {
        if let Some(child) = obj.get_mut(key) {
            if let Some(children) = child.as_array_mut() {
                for child in children {
                    rebase(child, enabled, legacy, depth + 1)?;
                }
            } else {
                rebase(child, enabled, legacy, depth + 1)?;
            }
        }
    }
    Ok(())
}
pub(crate) fn wrap(body: &mut Value) -> PluginResult<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    if let Some(tools) = body["tools"].as_array_mut() {
        for tool in tools {
            let Some(name) = tool["name"].as_str().map(str::to_owned) else {
                continue;
            };
            let mut schema = tool["input_schema"].clone();
            let Some(obj) = schema.as_object() else {
                continue;
            };
            if !["anyOf", "oneOf", "allOf"]
                .iter()
                .any(|k| obj.contains_key(*k))
            {
                continue;
            }
            let legacy = obj
                .get("$schema")
                .and_then(Value::as_str)
                .is_some_and(|s| s.contains("draft-03") || s.contains("draft-04"));
            rebase(&mut schema, true, legacy, 0)?;
            let dialect = schema.get("$schema").cloned();
            tool["input_schema"] = json!({"type":"object","properties":{"arguments":schema},"required":["arguments"],"additionalProperties":false});
            if let Some(dialect) = dialect {
                tool["input_schema"]["$schema"] = dialect;
            }
            if let Some(examples) = tool["input_examples"].as_array_mut() {
                for input in examples {
                    *input = json!({"arguments":input.take()});
                }
            }
            names.insert(name);
        }
    }
    if let Some(messages) = body["messages"].as_array_mut() {
        for message in messages {
            if message["role"] != "assistant" {
                continue;
            }
            if let Some(blocks) = message["content"].as_array_mut() {
                for block in blocks {
                    if block["type"] == "tool_use"
                        && block["name"].as_str().is_some_and(|s| names.contains(s))
                        && block.get("input").is_some()
                    {
                        block["input"] = json!({"arguments":block["input"].take()});
                    }
                }
            }
        }
    }
    Ok(names)
}
pub(crate) fn unwrap(value: Value) -> PluginResult<Value> {
    let Value::Object(mut object) = value else {
        return Err(malformed());
    };
    if object.len() != 1 {
        return Err(malformed());
    }
    object.remove("arguments").ok_or_else(malformed)
}
pub(crate) fn malformed() -> PluginError {
    PluginError::transport("Factory returned invalid or incomplete wrapped tool arguments.")
}
pub(crate) fn response(value: &mut Value, names: &BTreeSet<String>) -> PluginResult<()> {
    if let Some(blocks) = value["content"].as_array_mut() {
        for block in blocks {
            if block["type"] == "tool_use"
                && block["name"].as_str().is_some_and(|s| names.contains(s))
            {
                block["input"] = unwrap(block["input"].take())?;
            }
        }
    }
    Ok(())
}
#[cfg(test)]
#[path = "../test/schema.test.rs"]
mod tests;
