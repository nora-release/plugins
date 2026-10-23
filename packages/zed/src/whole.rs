use super::request::Wire;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::BTreeMap;
fn invalid() -> PluginError {
    PluginError::transport("Zed returned an incomplete provider reply or tool call.")
}
pub(crate) fn assemble(wire: Wire, events: &[Value]) -> PluginResult<Value> {
    match wire {
        Wire::Anthropic => anthropic(events),
        Wire::Responses => responses(events),
        Wire::Chat => chat(events),
        Wire::Gemini => gemini(events),
    }
}
fn index(value: &Value, max: u64) -> PluginResult<usize> {
    let n = if value.is_null() {
        0
    } else {
        value.as_u64().ok_or_else(invalid)?
    };
    if n > max {
        return Err(invalid());
    }
    Ok(n as usize)
}
fn append(target: &mut Value, key: &str, text: Option<&str>) {
    if let Some(text) = text {
        let previous = target[key].as_str().unwrap_or_default();
        target[key] = json!(format!("{previous}{text}"));
    }
}
fn anthropic(events: &[Value]) -> PluginResult<Value> {
    let mut message = None;
    let mut blocks = BTreeMap::new();
    let mut arguments = BTreeMap::<usize, String>::new();
    let mut ended = false;
    for event in events {
        match event["type"].as_str() {
            Some("message_start") => {
                let mut msg = event["message"].clone();
                if !msg.is_object() {
                    return Err(invalid());
                }
                msg["content"] = json!([]);
                message = Some(msg);
            }
            Some("content_block_start") => {
                let i = index(&event["index"], 1023)?;
                let block = event["content_block"].clone();
                if block["type"] == "tool_use" {
                    arguments.insert(i, String::new());
                }
                if blocks.insert(i, block).is_some() {
                    return Err(invalid());
                }
            }
            Some("content_block_delta") => {
                let i = index(&event["index"], 1023)?;
                let block = blocks.get_mut(&i).ok_or_else(invalid)?;
                let delta = &event["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => append(block, "text", delta["text"].as_str()),
                    Some("thinking_delta") => append(block, "thinking", delta["thinking"].as_str()),
                    Some("signature_delta") => {
                        append(block, "signature", delta["signature"].as_str())
                    }
                    Some("input_json_delta") => arguments
                        .get_mut(&i)
                        .ok_or_else(invalid)?
                        .push_str(delta["partial_json"].as_str().ok_or_else(invalid)?),
                    _ => {}
                }
            }
            Some("message_delta") => {
                if let Some(message) = message.as_mut() {
                    if let (Some(message), Some(delta)) =
                        (message.as_object_mut(), event["delta"].as_object())
                    {
                        message.extend(delta.clone());
                    }
                    if let Some(usage) = event["usage"].as_object() {
                        if !message["usage"].is_object() {
                            message["usage"] = json!({});
                        }
                        message["usage"]
                            .as_object_mut()
                            .unwrap()
                            .extend(usage.clone());
                    }
                }
            }
            Some("message_stop") => ended = true,
            _ => {}
        }
    }
    let mut message = message.ok_or_else(invalid)?;
    if !ended && message["stop_reason"].is_null() {
        return Err(invalid());
    }
    for (i, args) in arguments {
        let block = blocks.get_mut(&i).ok_or_else(invalid)?;
        if !args.is_empty() {
            block["input"] = serde_json::from_str(&args).map_err(|_| invalid())?;
        }
        if !block["input"].is_object()
            || block["id"].as_str().is_none_or(str::is_empty)
            || block["name"].as_str().is_none_or(str::is_empty)
        {
            return Err(invalid());
        }
    }
    message["content"] = json!(blocks.into_values().collect::<Vec<_>>());
    Ok(message)
}
fn responses(events: &[Value]) -> PluginResult<Value> {
    events
        .iter()
        .rev()
        .find(|e| {
            ["response.completed", "response.incomplete"]
                .contains(&e["type"].as_str().unwrap_or_default())
        })
        .and_then(|e| e.get("response"))
        .filter(|r| r.is_object())
        .cloned()
        .ok_or_else(invalid)
}
fn chat(events: &[Value]) -> PluginResult<Value> {
    let mut out = json!({"object":"chat.completion"});
    let mut choices = BTreeMap::<usize, Value>::new();
    let mut calls = BTreeMap::<(usize, usize), Value>::new();
    for event in events {
        for key in ["id", "created", "model", "system_fingerprint"] {
            if out.get(key).is_none() {
                if let Some(value) = event.get(key) {
                    out[key] = value.clone();
                }
            }
        }
        if let Some(usage) = event.get("usage") {
            out["usage"] = usage.clone();
        }
        for choice in event["choices"].as_array().into_iter().flatten() {
            let i = index(&choice["index"], 31)?;
            let ch=choices.entry(i).or_insert_with(||json!({"index":i,"message":{"role":"assistant","content":""},"finish_reason":null}));
            let delta = &choice["delta"];
            for key in ["content", "reasoning_content", "reasoning"] {
                append(&mut ch["message"], key, delta[key].as_str());
            }
            for call in delta["tool_calls"].as_array().into_iter().flatten() {
                let j = index(&call["index"], 255)?;
                let target = calls.entry((i, j)).or_insert_with(
                    || json!({"id":"","type":"function","function":{"name":"","arguments":""}}),
                );
                if let Some(id) = call["id"].as_str() {
                    if !target["id"].as_str().unwrap_or_default().is_empty() && target["id"] != id {
                        return Err(invalid());
                    }
                    target["id"] = json!(id);
                }
                append(
                    &mut target["function"],
                    "name",
                    call["function"]["name"].as_str(),
                );
                append(
                    &mut target["function"],
                    "arguments",
                    call["function"]["arguments"].as_str(),
                );
            }
            if let Some(reason) = choice["finish_reason"].as_str() {
                ch["finish_reason"] = json!(reason);
            }
        }
    }
    if choices.is_empty() {
        return Err(invalid());
    }
    for ((i, _), call) in calls {
        if call["id"].as_str().is_none_or(str::is_empty)
            || call["function"]["name"].as_str().is_none_or(str::is_empty)
        {
            return Err(invalid());
        }
        let args = call["function"]["arguments"].as_str().unwrap_or_default();
        if !args.is_empty()
            && !serde_json::from_str::<Value>(args)
                .map(|v| v.is_object())
                .unwrap_or(false)
        {
            return Err(invalid());
        }
        let choice = choices.get_mut(&i).ok_or_else(invalid)?;
        if choice["message"]["tool_calls"].is_null() {
            choice["message"]["tool_calls"] = json!([]);
        }
        choice["message"]["tool_calls"]
            .as_array_mut()
            .unwrap()
            .push(call);
    }
    if choices
        .values()
        .any(|c| c["finish_reason"].as_str().is_none_or(str::is_empty))
    {
        return Err(invalid());
    }
    out["choices"] = json!(choices.into_values().collect::<Vec<_>>());
    Ok(out)
}
fn gemini(events: &[Value]) -> PluginResult<Value> {
    let mut out = json!({"candidates":[]});
    let mut candidates = BTreeMap::<usize, Value>::new();
    for event in events {
        for key in [
            "usageMetadata",
            "modelVersion",
            "promptFeedback",
            "responseId",
        ] {
            if let Some(value) = event.get(key) {
                out[key] = value.clone();
            }
        }
        for (default, candidate) in event["candidates"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let i = if candidate.get("index").is_some() {
                index(&candidate["index"], 31)?
            } else {
                default
            };
            let target = candidates
                .entry(i)
                .or_insert_with(|| json!({"index":i,"content":{"role":"model","parts":[]}}));
            for part in candidate["content"]["parts"]
                .as_array()
                .into_iter()
                .flatten()
            {
                let parts = target["content"]["parts"].as_array_mut().unwrap();
                let merge = part["text"].is_string()
                    && part
                        .as_object()
                        .is_some_and(|o| o.keys().all(|k| k == "text" || k == "thought"))
                    && parts.last().is_some_and(|last| {
                        last["text"].is_string()
                            && last["thought"].as_bool().unwrap_or(false)
                                == part["thought"].as_bool().unwrap_or(false)
                            && last
                                .as_object()
                                .is_some_and(|o| o.keys().all(|k| k == "text" || k == "thought"))
                    });
                if merge {
                    append(parts.last_mut().unwrap(), "text", part["text"].as_str());
                } else {
                    parts.push(part.clone());
                }
            }
            for key in [
                "finishReason",
                "safetyRatings",
                "citationMetadata",
                "groundingMetadata",
            ] {
                if let Some(value) = candidate.get(key) {
                    target[key] = value.clone();
                }
            }
        }
    }
    if candidates.is_empty() && out["promptFeedback"].is_null() {
        return Err(invalid());
    }
    if candidates
        .values()
        .any(|c| c["finishReason"].as_str().is_none_or(str::is_empty))
    {
        return Err(invalid());
    }
    out["candidates"] = json!(candidates.into_values().collect::<Vec<_>>());
    Ok(out)
}
#[cfg(test)]
#[path = "../test/whole.test.rs"]
mod tests;
