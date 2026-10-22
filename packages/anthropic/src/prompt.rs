use super::{cli, mcp};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
pub(crate) fn parts(content: &Value) -> Vec<Value> {
    if let Some(text) = content.as_str() {
        vec![json!({"type":"text","text":text})]
    } else {
        content.as_array().cloned().unwrap_or_default()
    }
}
pub(crate) fn canonical(messages: &[Value]) -> Value {
    let mut output = Vec::new();
    for message in messages {
        let parts = parts(&message["content"])
            .into_iter()
            .filter_map(|mut part| {
                if ["thinking", "redacted_thinking"]
                    .contains(&part["type"].as_str().unwrap_or_default())
                {
                    return None;
                }
                if let Some(object) = part.as_object_mut() {
                    object.remove("cache_control");
                }
                Some(part)
            })
            .collect::<Vec<_>>();
        output.push(json!({"role":message["role"],"content":parts}));
    }
    json!(output)
}
fn text_block(text: String) -> Value {
    json!({"type":"text","text":text})
}
fn render(parts: &[Value], blocks: &mut Vec<Value>, buffer: &mut String) -> PluginResult<()> {
    for part in parts {
        match part["type"].as_str().unwrap_or_default() {
            "text" => {
                buffer.push_str(part["text"].as_str().unwrap_or_default());
                buffer.push('\n');
            }
            "thinking" | "redacted_thinking" => {}
            "image" => {
                if part.pointer("/source/type").and_then(Value::as_str) != Some("base64") {
                    return Err(PluginError::unsupported("URL-only Claude images"));
                }
                if !buffer.is_empty() {
                    blocks.push(text_block(std::mem::take(buffer)));
                }
                let mut image = part.clone();
                image.as_object_mut().unwrap().remove("cache_control");
                blocks.push(image);
            }
            "document" => {
                if !buffer.is_empty() {
                    blocks.push(text_block(std::mem::take(buffer)));
                }
                blocks.push(part.clone());
            }
            "tool_use" => {
                let name = cli::text(part.get("name"), 4096).ok_or_else(|| {
                    PluginError::invalid("The Claude history tool name is invalid.")
                })?;
                buffer.push_str(&format!(
                    "<previous_tool_call id={}>\n{}{} {}\n</previous_tool_call>\n",
                    serde_json::to_string(&part["id"]).unwrap(),
                    mcp::PREFIX,
                    mcp::tool_name(name),
                    part["input"]
                ));
            }
            "tool_result" => {
                buffer.push_str(&format!(
                    "<previous_tool_result id={} status=\"{}\">\n",
                    serde_json::to_string(&part["tool_use_id"]).unwrap(),
                    if part["is_error"] == true {
                        "error"
                    } else {
                        "success"
                    }
                ));
                render(&parts_of_result(&part["content"]), blocks, buffer)?;
                buffer.push_str("</previous_tool_result>\n");
            }
            _ => return Err(PluginError::unsupported("Claude message content")),
        }
    }
    Ok(())
}
fn parts_of_result(content: &Value) -> Vec<Value> {
    if let Some(text) = content.as_str() {
        vec![text_block(text.into())]
    } else {
        parts(content)
    }
}
pub(crate) fn initial(request: &Value) -> PluginResult<Vec<Value>> {
    let messages = request["messages"]
        .as_array()
        .ok_or_else(|| PluginError::invalid("Claude requires Anthropic messages."))?;
    if messages.len() > 10000 {
        return Err(PluginError::invalid(
            "The Claude conversation has too many messages.",
        ));
    }
    let mut blocks = Vec::new();
    let mut buffer = String::new();
    let mut system = parts(&request["system"])
        .into_iter()
        .filter_map(|part| part["text"].as_str().map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(schema) = request
        .pointer("/output_config/format")
        .filter(|format| format["type"] == "json_schema")
        .and_then(|format| format.get("schema"))
        .filter(|schema| schema.is_object())
    {
        system.push_str(&format!(
            "\nRespond with a single JSON value matching this schema: {schema}"
        ));
    }
    if !system.is_empty() {
        blocks.push(json!({"type":"text","text":format!("<external_system_instructions>\n{system}\n</external_system_instructions>\n"),"cache_control":{"type":"ephemeral","ttl":"1h"}}));
    }
    let split = messages
        .iter()
        .enumerate()
        .rfind(|(_, message)| {
            message["role"] == "assistant"
                && !parts(&message["content"])
                    .iter()
                    .any(|part| part["type"] == "tool_use")
        })
        .map(|(index, _)| index + 1)
        .filter(|index| *index < messages.len())
        .unwrap_or(0);
    if split > 0 {
        buffer.push_str("<previous_conversation>\nThe following messages were already answered. Continue after this history.\n");
    }
    for (index, message) in messages.iter().enumerate() {
        if split > 0 && index == split {
            buffer.push_str("</previous_conversation>\n<current_turn>\n");
        }
        buffer.push_str(if message["role"] == "assistant" {
            "Assistant: "
        } else {
            "Human: "
        });
        render(&parts(&message["content"]), &mut blocks, &mut buffer)?;
        buffer.push_str("\n\n");
    }
    if split > 0 {
        buffer.push_str("</current_turn>\n");
    }
    if !buffer.trim().is_empty() {
        blocks.push(text_block(buffer));
    }
    if blocks.is_empty() {
        blocks.push(text_block("Please proceed with the task.".into()));
    }
    Ok(blocks)
}
pub(crate) fn turn(messages: &[Value]) -> PluginResult<Vec<Value>> {
    let mut blocks = Vec::new();
    let mut buffer = String::new();
    for message in messages {
        render(&parts(&message["content"]), &mut blocks, &mut buffer)?;
    }
    if !buffer.is_empty() {
        blocks.push(text_block(buffer));
    }
    Ok(blocks)
}
pub(crate) fn results(messages: &[Value]) -> PluginResult<Vec<(String, Value)>> {
    let mut output = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for message in messages {
        if message["role"] != "user" {
            return Err(PluginError::invalid(
                "Claude tool results must come from the caller.",
            ));
        }
        for part in parts(&message["content"]) {
            if part["type"] == "tool_result" {
                let id = cli::text(part.get("tool_use_id"), 256).ok_or_else(|| {
                    PluginError::invalid("A Claude tool result has no call identifier.")
                })?;
                if !seen.insert(id.to_owned()) {
                    return Err(PluginError::invalid(
                        "A Claude tool result was supplied twice.",
                    ));
                }
                output.push((id.into(), mcp::result(&part)?));
            }
        }
    }
    Ok(output)
}
#[cfg(test)]
#[path = "../test/prompt.test.rs"]
mod tests;
