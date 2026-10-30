use super::session::{bound_call_id, canonical_json};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

pub(super) fn normalize(mut input: Vec<Value>) -> Vec<Value> {
    let mut calls = BTreeMap::new();
    for item in &mut input {
        let kind = item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let id = item.get("call_id").and_then(Value::as_str).unwrap_or("");
        let bounded = bound_call_id(id);
        if matches!(
            kind.as_str(),
            "function_call"
                | "function_call_output"
                | "local_shell_call"
                | "local_shell_call_output"
                | "custom_tool_call"
                | "custom_tool_call_output"
                | "tool_search_call"
                | "tool_search_output"
        ) {
            if id != bounded {
                item["call_id"] = json!(bounded);
            }
        }
        let output = match kind.as_str() {
            "function_call" => "function_call_output",
            "custom_tool_call" => "custom_tool_call_output",
            "local_shell_call" => "local_shell_call_output",
            _ => continue,
        };
        calls.insert(bounded.trim().to_owned(), output);
    }
    input
        .into_iter()
        .filter_map(|item| normalize_item(item, &calls))
        .collect()
}

fn normalize_item(item: Value, calls: &BTreeMap<String, &str>) -> Option<Value> {
    let Value::Object(mut item) = item else {
        return Some(item);
    };
    let kind = item
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if kind == "item_reference" {
        return None;
    }
    if matches!(
        kind.as_str(),
        "function_call_output" | "custom_tool_call_output"
    ) && item
        .get("call_id")
        .is_none_or(|id| id.is_null() || id == "")
    {
        return Some(Value::Object(item));
    }
    item.remove("id");
    if matches!(kind.as_str(), "" | "message" | "reasoning") {
        item.remove("status");
    }
    if matches!(
        kind.as_str(),
        "function_call_output" | "custom_tool_call_output" | "local_shell_call_output"
    ) {
        let id = item
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let paired = calls.get(id).is_some_and(|expected| {
            *expected == kind
                || (kind == "function_call_output" && *expected == "local_shell_call_output")
        });
        if !paired {
            return Some(orphan_output(&item, id));
        }
    }
    if item.get("role").and_then(Value::as_str) == Some("system")
        && matches!(kind.as_str(), "" | "message")
    {
        item.insert("role".into(), json!("developer"));
    }
    Some(Value::Object(item))
}

fn orphan_output(item: &Map<String, Value>, id: &str) -> Value {
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .unwrap_or("tool");
    let id = if id.is_empty() { "unknown" } else { id };
    let output = item.get("output").unwrap_or(&Value::Null);
    let mut text = output
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| canonical_json(output));
    if text.len() > 16_000 {
        let mut end = 16_000;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n...[truncated]");
    }
    json!({
        "type":"message", "role":"assistant",
        "content":format!("[Previous {name} result; call_id={id}]: {text}")
    })
}
