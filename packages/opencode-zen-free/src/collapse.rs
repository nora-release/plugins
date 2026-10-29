use crate::{protocol::Protocol, sse::Frame};
use futures::{stream::BoxStream, StreamExt};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::BTreeMap;
fn invalid(message: &str) -> PluginError {
    PluginError::transport(message)
}
fn index(value: &Value) -> PluginResult<u64> {
    if value.is_null() {
        return Ok(0);
    }
    value
        .as_u64()
        .filter(|n| *n <= 65535)
        .ok_or_else(|| invalid("Zen returned an invalid item index."))
}
fn append(target: &mut Value, field: &str, value: &Value) -> PluginResult<()> {
    if value.is_null() {
        return Ok(());
    }
    let text = value
        .as_str()
        .ok_or_else(|| invalid("Zen returned an invalid text delta."))?;
    let previous = target[field].as_str().unwrap_or("");
    target[field] = json!(format!("{previous}{text}"));
    Ok(())
}
fn set_part(value: &mut Value, field: &str, position: u64, part: Value) -> PluginResult<()> {
    if value[field].is_null() {
        value[field] = json!([]);
    }
    let parts = value[field]
        .as_array_mut()
        .ok_or_else(|| invalid("Zen returned an invalid content list."))?;
    let position = position as usize;
    if parts.len() <= position {
        parts.resize(position + 1, Value::Null);
    }
    parts[position] = part;
    Ok(())
}
fn part_mut<'a>(
    value: &'a mut Value,
    field: &str,
    position: u64,
    default: Value,
) -> PluginResult<&'a mut Value> {
    if value[field].is_null() {
        value[field] = json!([]);
    }
    let parts = value[field]
        .as_array_mut()
        .ok_or_else(|| invalid("Zen returned an invalid content list."))?;
    let position = position as usize;
    if parts.len() <= position {
        parts.resize(position + 1, Value::Null);
    }
    if parts[position].is_null() {
        parts[position] = default;
    }
    Ok(&mut parts[position])
}
struct Accumulator {
    protocol: Protocol,
    body: Value,
    items: BTreeMap<u64, Value>,
    tools: BTreeMap<(u64, u64), Value>,
    arguments: BTreeMap<u64, String>,
    final_body: Option<Value>,
}
impl Accumulator {
    fn new(protocol: Protocol) -> Self {
        Self {
            protocol,
            body: match protocol {
                Protocol::Chat => json!({"object":"chat.completion","choices":[]}),
                Protocol::Responses => json!({"object":"response","output":[]}),
                Protocol::Anthropic => {
                    json!({"type":"message","role":"assistant","content":[],"stop_reason":null,"stop_sequence":null})
                }
            },
            items: BTreeMap::new(),
            tools: BTreeMap::new(),
            arguments: BTreeMap::new(),
            final_body: None,
        }
    }
    fn usage(&mut self, data: &Value) {
        if let Some(usage) = data["usage"].as_object() {
            if !self.body["usage"].is_object() {
                self.body["usage"] = json!({});
            }
            self.body["usage"]
                .as_object_mut()
                .unwrap()
                .extend(usage.clone());
        }
    }
    fn output(&mut self, data: &Value, kind: &str) -> PluginResult<&mut Value> {
        let position = if data["output_index"].is_null() {
            data["item_id"]
                .as_str()
                .and_then(|id| {
                    self.items
                        .iter()
                        .find(|(_, value)| value["id"] == id)
                        .map(|(index, _)| *index)
                })
                .unwrap_or(0)
        } else {
            index(&data["output_index"])?
        };
        Ok(self.items.entry(position).or_insert_with(|| {
            let mut value = json!({"type":kind});
            if !data["item_id"].is_null() {
                value["id"] = data["item_id"].clone();
            }
            if kind == "message" {
                value["role"] = json!("assistant");
                value["content"] = json!([]);
            } else if kind == "function_call" {
                value["arguments"] = json!("");
            } else if kind == "reasoning" {
                value["summary"] = json!([]);
            }
            value
        }))
    }
    fn apply(&mut self, data: &Value, kind: &str) -> PluginResult<()> {
        match self.protocol {
            Protocol::Chat => {
                for key in [
                    "id",
                    "created",
                    "model",
                    "system_fingerprint",
                    "service_tier",
                ] {
                    if let Some(value) = data.get(key) {
                        self.body[key] = value.clone();
                    }
                }
                self.usage(data);
                for delta in data["choices"].as_array().into_iter().flatten() {
                    let position = index(&delta["index"])?;
                    let choice=self.items.entry(position).or_insert_with(||json!({"index":position,"message":{"role":"assistant","content":null},"finish_reason":null}));
                    let message = &mut choice["message"];
                    let fragment = &delta["delta"];
                    if let Some(role) = fragment["role"].as_str() {
                        message["role"] = json!(role);
                    }
                    for key in ["content", "reasoning_content", "reasoning", "refusal"] {
                        append(message, key, &fragment[key])?;
                    }
                    if !fragment["function_call"].is_null() {
                        if message["function_call"].is_null() {
                            message["function_call"] = json!({});
                        }
                        for key in ["name", "arguments"] {
                            append(
                                &mut message["function_call"],
                                key,
                                &fragment["function_call"][key],
                            )?;
                        }
                    }
                    for fragment in fragment["tool_calls"].as_array().into_iter().flatten() {
                        let tool = index(&fragment["index"])?;
                        let call = self.tools.entry((position, tool)).or_insert_with(
                            || json!({"type":"function","function":{"name":"","arguments":""}}),
                        );
                        for key in ["id", "type"] {
                            if !fragment[key].is_null() {
                                call[key] = fragment[key].clone();
                            }
                        }
                        for key in ["name", "arguments"] {
                            append(&mut call["function"], key, &fragment["function"][key])?;
                        }
                    }
                    if !delta["finish_reason"].is_null() {
                        choice["finish_reason"] = delta["finish_reason"].clone();
                    }
                    if let Some(logprobs) = delta["logprobs"].as_object() {
                        if !choice["logprobs"].is_object() {
                            choice["logprobs"] = json!({});
                        }
                        for (key, value) in logprobs {
                            if let Some(values) = value.as_array() {
                                if !choice["logprobs"][key].is_array() {
                                    choice["logprobs"][key] = json!([]);
                                }
                                choice["logprobs"][key]
                                    .as_array_mut()
                                    .unwrap()
                                    .extend(values.clone());
                            } else {
                                choice["logprobs"][key] = value.clone();
                            }
                        }
                    }
                }
            }
            Protocol::Responses => {
                match kind {
                    "response.created" | "response.in_progress" => {
                        if let Some(response) = data["response"].as_object() {
                            self.body.as_object_mut().unwrap().extend(response.clone());
                            for (position, value) in data["response"]["output"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .enumerate()
                            {
                                self.items.insert(position as u64, value.clone());
                            }
                        }
                    }
                    "response.output_item.added" | "response.output_item.done" => {
                        if !data["item"].is_null() {
                            self.items
                                .insert(index(&data["output_index"])?, data["item"].clone());
                        }
                    }
                    "response.content_part.added" | "response.content_part.done" => {
                        set_part(
                            self.output(data, "message")?,
                            "content",
                            index(&data["content_index"])?,
                            data["part"].clone(),
                        )?;
                    }
                    "response.output_text.delta"
                    | "response.output_text.done"
                    | "response.refusal.delta"
                    | "response.refusal.done" => {
                        let refusal = kind.starts_with("response.refusal.");
                        let field = if refusal { "refusal" } else { "text" };
                        let part = part_mut(
                            self.output(data, "message")?,
                            "content",
                            index(&data["content_index"])?,
                            if refusal {
                                json!({"type":"refusal","refusal":""})
                            } else {
                                json!({"type":"output_text","text":"","annotations":[]})
                            },
                        )?;
                        if kind.ends_with(".delta") {
                            append(part, field, &data["delta"])?;
                        } else if !data[field].is_null() {
                            part[field] = data[field].clone();
                        }
                    }
                    "response.function_call_arguments.delta"
                    | "response.function_call_arguments.done" => {
                        let item = self.output(data, "function_call")?;
                        if kind.ends_with(".delta") {
                            append(item, "arguments", &data["delta"])?;
                        } else if !data["arguments"].is_null() {
                            item["arguments"] = data["arguments"].clone();
                        }
                    }
                    "response.reasoning_summary_part.added"
                    | "response.reasoning_summary_part.done" => {
                        set_part(
                            self.output(data, "reasoning")?,
                            "summary",
                            index(&data["summary_index"])?,
                            data["part"].clone(),
                        )?;
                    }
                    "response.reasoning_text.delta"
                    | "response.reasoning_text.done"
                    | "response.reasoning_summary_text.delta"
                    | "response.reasoning_summary_text.done" => {
                        let summary = kind.contains("summary");
                        let (field, position) = if summary {
                            ("summary", index(&data["summary_index"])?)
                        } else {
                            ("content", index(&data["content_index"])?)
                        };
                        let part = part_mut(
                            self.output(data, "reasoning")?,
                            field,
                            position,
                            json!({"type":if summary{"summary_text"}else{"reasoning_text"},"text":""}),
                        )?;
                        if kind.ends_with(".delta") {
                            append(part, "text", &data["delta"])?;
                        } else if !data["text"].is_null() {
                            part["text"] = data["text"].clone();
                        }
                    }
                    "response.reasoning_content.delta" => {
                        append(&mut self.body, "reasoning_content", &data["delta"])?
                    }
                    "response.completed" | "response.incomplete" => {
                        if data["response"].is_object() {
                            self.final_body = Some(data["response"].clone());
                        }
                        self.body["status"] = json!(if kind == "response.completed" {
                            "completed"
                        } else {
                            "incomplete"
                        });
                        for key in ["incomplete_details", "finish_reason"] {
                            if !data[key].is_null() {
                                self.body[key] = data[key].clone();
                            }
                        }
                    }
                    _ => {}
                }
                self.usage(data);
            }
            Protocol::Anthropic => match kind {
                "message_start" => {
                    if let Some(message) = data["message"].as_object() {
                        self.body.as_object_mut().unwrap().extend(message.clone());
                        for (position, value) in data["message"]["content"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .enumerate()
                        {
                            self.items.insert(position as u64, value.clone());
                        }
                    }
                }
                "content_block_start" => {
                    self.items
                        .insert(index(&data["index"])?, data["content_block"].clone());
                }
                "content_block_delta" => {
                    let position = index(&data["index"])?;
                    let block = self
                        .items
                        .get_mut(&position)
                        .ok_or_else(|| invalid("Zen returned a delta without a content block."))?;
                    let delta = &data["delta"];
                    match delta["type"].as_str() {
                        Some("text_delta") => append(block, "text", &delta["text"])?,
                        Some("thinking_delta") => append(block, "thinking", &delta["thinking"])?,
                        Some("signature_delta") => append(block, "signature", &delta["signature"])?,
                        Some("input_json_delta") => {
                            let text = delta["partial_json"].as_str().ok_or_else(|| {
                                invalid("Zen returned an invalid tool input delta.")
                            })?;
                            self.arguments.entry(position).or_default().push_str(text);
                        }
                        _ => {}
                    }
                }
                "message_delta" => {
                    if let Some(delta) = data["delta"].as_object() {
                        self.body.as_object_mut().unwrap().extend(delta.clone());
                    }
                    self.usage(data);
                }
                _ => {}
            },
        }
        Ok(())
    }
    fn finish(mut self) -> PluginResult<Value> {
        if let Some(value) = self.final_body {
            return Ok(value);
        }
        match self.protocol {
            Protocol::Chat => {
                for ((choice, _), tool) in self.tools {
                    let choice = self
                        .items
                        .get_mut(&choice)
                        .ok_or_else(|| invalid("Zen returned tools without a choice."))?;
                    if !choice["message"]["tool_calls"].is_array() {
                        choice["message"]["tool_calls"] = json!([]);
                    }
                    choice["message"]["tool_calls"]
                        .as_array_mut()
                        .unwrap()
                        .push(tool);
                }
                self.body["choices"] = json!(self.items.into_values().collect::<Vec<_>>());
            }
            Protocol::Responses => {
                self.body["output"] = json!(self.items.into_values().collect::<Vec<_>>())
            }
            Protocol::Anthropic => {
                for (position, arguments) in self.arguments {
                    let input = serde_json::from_str::<Value>(&arguments)
                        .map_err(|_| invalid("Zen returned invalid JSON tool input."))?;
                    self.items
                        .get_mut(&position)
                        .ok_or_else(|| invalid("Zen returned tool input without a block."))?
                        ["input"] = input;
                }
                self.body["content"] = json!(self.items.into_values().collect::<Vec<_>>());
            }
        }
        Ok(self.body)
    }
}
pub(crate) async fn collapse(
    mut frames: BoxStream<'static, PluginResult<Frame>>,
    protocol: Protocol,
) -> PluginResult<Value> {
    let mut accumulator = Accumulator::new(protocol);
    let mut completed = false;
    let mut bytes = 0usize;
    while let Some(frame) = frames.next().await {
        let frame = frame?;
        bytes = bytes.saturating_add(frame.encoded().len());
        if bytes > 32 * 1024 * 1024 {
            return Err(invalid("Zen's response exceeds 32 MiB."));
        }
        if frame.text.trim().is_empty() {
            continue;
        }
        if frame.text.trim() == "[DONE]" {
            if protocol == Protocol::Chat {
                completed = true;
            }
            continue;
        }
        let value = frame.value()?;
        let kind = value["type"].as_str().unwrap_or(&frame.name);
        if frame.name == "error"
            || kind == "error"
            || !value["error"].is_null()
            || kind == "response.failed"
            || value["response"]["status"] == "failed"
        {
            return Err(invalid("Zen returned an upstream error."));
        }
        if completed {
            continue;
        }
        accumulator.apply(&value, kind)?;
        if (protocol == Protocol::Responses
            && ["response.completed", "response.incomplete"].contains(&kind))
            || (protocol == Protocol::Anthropic && kind == "message_stop")
        {
            completed = true;
        }
    }
    if !completed {
        return Err(invalid("Zen's SSE ended without a completion marker."));
    }
    accumulator.finish()
}
#[cfg(test)]
#[path = "../test/collapse.test.rs"]
mod tests;
