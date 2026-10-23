use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
fn refs(root: &Value, branch: &Value) -> Option<Value> {
    let Some(reference) = branch["$ref"].as_str().filter(|s| !s.is_empty()) else {
        return branch.is_object().then(|| branch.clone());
    };
    for key in ["$defs", "definitions"] {
        if let Some(name) = reference.strip_prefix(&format!("#/{key}/")) {
            return root[key].get(name).filter(|v| v.is_object()).cloned();
        }
    }
    None
}
pub(crate) fn object_root(value: &Value) -> PluginResult<Value> {
    fold(value, value, 0)
}
fn fold(root: &Value, value: &Value, depth: usize) -> PluginResult<Value> {
    if depth > 8 {
        return Err(PluginError::invalid(
            "The Devin tool schema has too many root compositions.",
        ));
    }
    let Some(object) = value.as_object() else {
        return Ok(value.clone());
    };
    if value["type"] == "object"
        && !["anyOf", "oneOf", "allOf"]
            .iter()
            .any(|k| object.contains_key(*k))
    {
        return Ok(value.clone());
    }
    let mut out = object.clone();
    let mut props = value["properties"].as_object().cloned().unwrap_or_default();
    let mut required = value["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    for branch in value["allOf"].as_array().into_iter().flatten() {
        let Some(branch) = refs(root, branch) else {
            continue;
        };
        for (k, v) in branch["properties"].as_object().into_iter().flatten() {
            props.entry(k.clone()).or_insert_with(|| v.clone());
        }
        required.extend(
            branch["required"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned),
        );
    }
    out.remove("allOf");
    let mut branches = Vec::new();
    for key in ["anyOf", "oneOf"] {
        for branch in value[key].as_array().into_iter().flatten() {
            let Some(mut branch) = refs(root, branch) else {
                continue;
            };
            if ["anyOf", "oneOf", "allOf"]
                .iter()
                .any(|k| branch.get(*k).is_some())
            {
                branch = fold(root, &branch, depth + 1)?;
            }
            if branch["type"] == "object" || branch.get("properties").is_some() {
                branches.push(branch);
            }
        }
        out.remove(key);
    }
    let mut common: Option<BTreeSet<String>> = None;
    let mut variants: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for branch in branches {
        for (k, v) in branch["properties"].as_object().into_iter().flatten() {
            if props.contains_key(k) {
                continue;
            }
            let values = variants.entry(k.clone()).or_default();
            if !values.contains(v) {
                values.push(v.clone());
            }
        }
        let keys = branch["required"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        common = Some(
            common
                .map(|old| old.intersection(&keys).cloned().collect())
                .unwrap_or(keys),
        );
    }
    for (k, mut variants) in variants {
        props.insert(
            k,
            if variants.len() == 1 {
                variants.remove(0)
            } else {
                json!({"anyOf":variants})
            },
        );
    }
    required.extend(common.unwrap_or_default());
    out.insert("type".into(), json!("object"));
    out.insert("properties".into(), json!(props));
    if required.is_empty() {
        out.remove("required");
    } else {
        out.insert("required".into(), json!(required));
    }
    Ok(json!(out))
}
pub(crate) fn gemini(value: &Value, depth: usize) -> PluginResult<Value> {
    if depth > 32 {
        return Err(PluginError::invalid(
            "The Devin Gemini tool schema is too deeply nested.",
        ));
    }
    let Some(mut out) = value.as_object().cloned() else {
        return Ok(value.clone());
    };
    for key in [
        "properties",
        "patternProperties",
        "$defs",
        "definitions",
        "dependentSchemas",
    ] {
        if let Some(children) = out.get_mut(key).and_then(Value::as_object_mut) {
            for child in children.values_mut() {
                *child = gemini(child, depth + 1)?;
            }
        }
    }
    for key in ["anyOf", "oneOf", "allOf", "prefixItems"] {
        if let Some(children) = out.get_mut(key).and_then(Value::as_array_mut) {
            for child in children {
                *child = gemini(child, depth + 1)?;
            }
        }
    }
    for key in [
        "additionalProperties",
        "not",
        "if",
        "then",
        "else",
        "contains",
    ] {
        if let Some(child) = out.get_mut(key).filter(|v| v.is_object()) {
            *child = gemini(child, depth + 1)?;
        }
    }
    if let Some(types) = out.get("type").and_then(Value::as_array) {
        let types = types
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        let some = types
            .iter()
            .filter(|t| *t != "null")
            .cloned()
            .collect::<Vec<_>>();
        if types.contains("null") && !some.is_empty() {
            out.insert("nullable".into(), json!(true));
        }
        match some.len() {
            0 => {
                if types.is_empty() {
                    out.remove("type");
                } else {
                    out.insert("type".into(), json!("null"));
                }
            }
            1 => {
                out.insert("type".into(), json!(some[0]));
            }
            _ => {
                out.remove("type");
                let mut branches = out
                    .get("anyOf")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for kind in some {
                    branches.push(if kind == "array" {
                        json!({"type":kind,"items":{}})
                    } else {
                        json!({"type":kind})
                    });
                }
                out.insert("anyOf".into(), json!(branches));
            }
        }
    }
    if !out.contains_key("type") {
        if let Some(values) = out.get("enum").and_then(Value::as_array).cloned() {
            let items = values
                .iter()
                .filter(|v| !v.is_null())
                .cloned()
                .collect::<Vec<_>>();
            if items.len() < values.len() {
                out.insert("nullable".into(), json!(true));
            }
            let kinds = items
                .iter()
                .map(|v| match v {
                    Value::String(_) => "string",
                    Value::Bool(_) => "boolean",
                    Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
                    Value::Number(_) => "number",
                    _ => "unsupported",
                })
                .collect::<BTreeSet<_>>();
            let kind = if kinds.len() == 1 {
                kinds.iter().copied().next().unwrap_or_default()
            } else if kinds.len() == 2 && kinds.contains("number") && kinds.contains("integer") {
                "number"
            } else {
                ""
            };
            if ["string", "integer", "number", "boolean"].contains(&kind) {
                out.insert("type".into(), json!(kind));
                out.insert("enum".into(), json!(items));
            } else {
                out.remove("enum");
            }
        }
    }
    if !out.contains_key("type") && out.get("properties").is_some_and(Value::is_object) {
        out.insert("type".into(), json!("object"));
    }
    if out.get("type") == Some(&json!("array"))
        || !out.contains_key("type") && out.contains_key("items")
    {
        let item = out.get("items");
        let item = if let Some(items) = item.and_then(Value::as_array) {
            if items.len() == 1 && items[0].is_object() {
                gemini(&items[0], depth + 1)?
            } else {
                json!({})
            }
        } else if let Some(item) = item.filter(|v| v.is_object()) {
            gemini(item, depth + 1)?
        } else {
            json!({})
        };
        out.insert("items".into(), item);
        out.insert("type".into(), json!("array"));
    }
    Ok(Value::Object(out))
}
#[cfg(test)]
#[path = "../test/schema.test.rs"]
mod tests;
