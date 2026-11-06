use crate::{
    protocol::Protocol,
    sse::{self, Frame},
};
use futures::{stream::BoxStream, StreamExt};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
const NOTE: &str = "(The model called a tool without a name. The call was left out.)";
const MAX_PENDING: usize = 32 * 1024 * 1024;
#[derive(Clone, Debug)]
pub(crate) struct Names {
    declared: BTreeSet<String>,
}
impl Names {
    pub(crate) fn check(&self, name: &Value) -> Option<String> {
        let name = name.as_str().filter(|name| !name.is_empty())?;
        if self.declared.contains(name) {
            return Some(name.to_owned());
        }
        let lower = name.to_lowercase();
        let matches: Vec<_> = self
            .declared
            .iter()
            .filter(|name| name.to_lowercase() == lower)
            .collect();
        if matches.len() == 1 {
            return Some(matches[0].to_owned());
        }
        if matches.len() > 1 {
            return Some(name.to_owned());
        }
        let bare = shape(name);
        if !bare.is_empty() {
            let matches: Vec<_> = self
                .declared
                .iter()
                .filter(|name| shape(name) == bare)
                .collect();
            if matches.len() == 1 {
                return Some(matches[0].to_owned());
            }
        }
        Some(name.to_owned())
    }
}
fn shape(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace() && !['_', '.', '-'].contains(c))
        .collect()
}
pub(crate) fn normalize(body: &mut Value, protocol: Protocol) -> PluginResult<Names> {
    if !body.is_object() {
        return Err(PluginError::invalid("Expected a Zen request object."));
    }
    if body.get("tools").is_some_and(|tools| !tools.is_array()) {
        return Err(PluginError::invalid("Zen tools must be an array."));
    }
    if body.get("tools").is_none() {
        body["tools"] = json!([]);
    }
    let tools = body["tools"].as_array().unwrap();
    if tools.len() > 4096 {
        return Err(PluginError::invalid(
            "Zen supports at most 4096 declared tools.",
        ));
    }
    let present: BTreeSet<String> = tools
        .iter()
        .filter_map(|tool| {
            if protocol == Protocol::Chat {
                tool["function"]["name"].as_str()
            } else {
                tool["name"].as_str()
            }
        })
        .map(str::to_owned)
        .collect();
    let mut declared = present.clone();
    if protocol == Protocol::Chat {
        for function in body["functions"].as_array().into_iter().flatten() {
            if let Some(name) = function["name"].as_str() {
                declared.insert(name.to_owned());
            }
        }
    }
    for (name, parameter) in [
        ("bash", "command"),
        ("edit", "filePath"),
        ("glob", "pattern"),
        ("grep", "pattern"),
        ("read", "filePath"),
    ] {
        if present.contains(name) {
            continue;
        }
        let parameters = json!({"type":"object","properties":{parameter:{"type":"string"}},"additionalProperties":false});
        let mut tool = json!({"name":name,"description":"Not available. Never call this tool."});
        let definition = tool.as_object_mut().unwrap();
        definition.insert(
            if protocol == Protocol::Anthropic {
                "input_schema"
            } else {
                "parameters"
            }
            .into(),
            parameters,
        );
        let tool = if protocol == Protocol::Chat {
            json!({"type":"function","function":tool})
        } else if protocol == Protocol::Responses {
            tool["type"] = json!("function");
            tool
        } else {
            tool
        };
        body["tools"].as_array_mut().unwrap().push(tool);
    }
    Ok(Names { declared })
}
fn invalid(message: &str) -> PluginError {
    PluginError::transport(message)
}
fn position(value: &Value) -> PluginResult<u64> {
    if value.is_null() {
        return Ok(0);
    }
    value
        .as_u64()
        .filter(|n| *n <= 65535)
        .ok_or_else(|| invalid("Zen returned an invalid stream item index."))
}
fn array(value: &Value) -> PluginResult<&[Value]> {
    if value.is_null() {
        return Ok(&[]);
    }
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| invalid("Zen returned an invalid tool snapshot."))
}
fn append_function(target: &mut Value, fragment: &Value) -> PluginResult<()> {
    let fragment = fragment
        .as_object()
        .ok_or_else(|| invalid("Zen returned an invalid function delta."))?;
    if !target.is_object() {
        *target = json!({});
    }
    for (key, value) in fragment {
        if ["name", "arguments"].contains(&key.as_str()) {
            if value.is_null() {
                continue;
            }
            let text = value
                .as_str()
                .ok_or_else(|| invalid("Zen returned an invalid function delta."))?;
            let previous = target[key].as_str().unwrap_or("");
            target[key] = json!(format!("{previous}{text}"));
        } else {
            target[key] = value.clone();
        }
    }
    Ok(())
}
fn chat_message(value: &mut Value, names: &Names) -> PluginResult<bool> {
    if !value.is_object() {
        return Ok(false);
    }
    let mut notes = 0;
    if let Some(call) = value.get_mut("function_call") {
        if !call.is_object() {
            return Err(invalid("Zen returned an invalid function call."));
        }
        if let Some(name) = names.check(&call["name"]) {
            call["name"] = json!(name);
        } else {
            value.as_object_mut().unwrap().remove("function_call");
            notes += 1;
        }
    }
    if let Some(calls) = value.get("tool_calls") {
        let mut keep = Vec::new();
        for mut call in array(calls)?.to_vec() {
            if !call.is_object()
                || (!call["type"].is_null() && call["type"] != "function")
                || !call["function"].is_object()
            {
                return Err(invalid("Zen returned an unsupported tool call."));
            }
            if let Some(name) = names.check(&call["function"]["name"]) {
                call["function"]["name"] = json!(name);
                keep.push(call);
            } else {
                notes += 1;
            }
        }
        if keep.is_empty() {
            value.as_object_mut().unwrap().remove("tool_calls");
        } else {
            value["tool_calls"] = json!(keep);
        }
    }
    if notes > 0 {
        let note = std::iter::repeat(NOTE)
            .take(notes)
            .collect::<Vec<_>>()
            .join("\n\n");
        if let Some(parts) = value["content"].as_array_mut() {
            parts.push(json!({"type":"text","text":note}));
        } else {
            let previous = value["content"].as_str().unwrap_or("");
            value["content"] = json!(format!(
                "{previous}{}{note}",
                if previous.is_empty() { "" } else { "\n\n" }
            ));
        }
    }
    Ok(notes > 0
        && value["function_call"].is_null()
        && value["tool_calls"]
            .as_array()
            .is_none_or(|calls| calls.is_empty()))
}
fn replacement(protocol: Protocol, call: &Value) -> Value {
    if protocol == Protocol::Anthropic {
        return json!({"type":"text","text":NOTE});
    }
    json!({"type":"message","id":call["id"].as_str().filter(|id|!id.is_empty()).map(str::to_owned).unwrap_or_else(||format!("msg_{}",call["call_id"].as_str().unwrap_or("dropped"))),"role":"assistant","status":call["status"].as_str().unwrap_or("completed"),"content":[{"type":"output_text","text":NOTE,"annotations":[]}]})
}
pub(crate) fn snapshot(
    value: &mut Value,
    protocol: Protocol,
    names: &Names,
) -> PluginResult<(usize, usize)> {
    if protocol == Protocol::Chat {
        if let Some(choices) = value
            .get_mut("choices")
            .filter(|choices| !choices.is_null())
        {
            for choice in choices
                .as_array_mut()
                .ok_or_else(|| invalid("Zen returned an invalid choices snapshot."))?
            {
                if let Some(message) = choice.get_mut("message") {
                    if chat_message(message, names)?
                        && [Some("tool_calls"), Some("function_call")]
                            .contains(&choice["finish_reason"].as_str())
                    {
                        choice["finish_reason"] = json!("stop");
                    }
                }
            }
        }
        return Ok((0, 0));
    }
    if !value.is_object() {
        return Err(invalid("Zen returned an invalid tool snapshot."));
    }
    let kind = value["type"].as_str().unwrap_or("").to_owned();
    if protocol == Protocol::Responses && kind.ends_with("_call") && kind != "function_call" {
        return Err(invalid(
            "Zen returned an unsupported upstream tool call type.",
        ));
    }
    if (protocol == Protocol::Responses && kind == "function_call")
        || (protocol == Protocol::Anthropic
            && ["tool_use", "server_tool_use"].contains(&kind.as_str()))
    {
        if let Some(name) = names.check(&value["name"]) {
            value["name"] = json!(name);
            return Ok((1, 0));
        }
        *value = replacement(protocol, value);
        return Ok((0, 1));
    }
    let mut kept = 0;
    let mut left = 0;
    for key in if protocol == Protocol::Responses {
        &["output", "content"][..]
    } else {
        &["content"][..]
    } {
        if value[*key].is_string() {
            continue;
        }
        if let Some(items) = value.get_mut(*key).filter(|items| !items.is_null()) {
            for item in items
                .as_array_mut()
                .ok_or_else(|| invalid("Zen returned an invalid snapshot list."))?
            {
                let (k, l) = snapshot(item, protocol, names)?;
                kept += k;
                left += l;
            }
        }
    }
    for key in if protocol == Protocol::Responses {
        &["response", "item", "part"][..]
    } else {
        &["message", "content_block"][..]
    } {
        if let Some(item) = value.get_mut(*key).filter(|item| !item.is_null()) {
            let (k, l) = snapshot(item, protocol, names)?;
            kept += k;
            left += l;
        }
    }
    if protocol == Protocol::Anthropic
        && left > 0
        && kept == 0
        && kind == "message"
        && value["stop_reason"] == "tool_use"
    {
        value["stop_reason"] = json!("end_turn");
    }
    Ok((kept, left))
}
#[derive(Default)]
struct Calls {
    metadata: Value,
    calls: BTreeMap<u64, Value>,
    legacy: Value,
    bytes: usize,
}
#[derive(Default)]
struct Chat {
    pending: BTreeMap<u64, Calls>,
    bytes: usize,
    wrote: BTreeSet<u64>,
    dropped: BTreeSet<u64>,
    open: BTreeSet<u64>,
    finished: BTreeSet<u64>,
    said: bool,
    metadata: Value,
}
impl Chat {
    fn complete(&mut self, index: u64, names: &Names) -> PluginResult<Value> {
        let Some(state) = self.pending.remove(&index) else {
            return Ok(json!({}));
        };
        self.bytes = self.bytes.saturating_sub(state.bytes);
        let mut delta = json!({});
        if !state.calls.is_empty() {
            delta["tool_calls"] = json!(state.calls.into_values().collect::<Vec<_>>());
        }
        if !state.legacy.is_null() {
            delta["function_call"] = state.legacy;
        }
        if chat_message(&mut delta, names)? {
            self.dropped.insert(index);
        }
        if delta.get("content").is_some() {
            if let Some(calls) = delta["tool_calls"].as_array_mut() {
                for (position, call) in calls.iter_mut().enumerate() {
                    call["index"] = json!(position);
                }
            }
        }
        if self.wrote.contains(&index) {
            if let Some(text) = delta["content"].as_str() {
                delta["content"] = json!(format!("\n\n{text}"));
            }
        }
        Ok(delta)
    }
    fn apply(&mut self, mut data: Value, names: &Names) -> PluginResult<Option<Value>> {
        snapshot(&mut data, Protocol::Chat, names)?;
        self.metadata = data.clone();
        self.metadata.as_object_mut().unwrap().remove("choices");
        self.metadata.as_object_mut().unwrap().remove("usage");
        if data["choices"].is_null() {
            data["choices"] = json!([]);
        }
        let choices = data["choices"]
            .as_array_mut()
            .ok_or_else(|| invalid("Zen returned invalid choices."))?;
        for choice in choices.iter_mut() {
            let index = position(&choice["index"])?;
            if let Some(text) = choice["delta"]["content"]
                .as_str()
                .filter(|text| !text.is_empty())
            {
                let _ = text;
                self.wrote.insert(index);
            }
            for field in ["content", "reasoning_content", "reasoning"] {
                if choice["delta"][field]
                    .as_str()
                    .is_some_and(|text| !text.is_empty())
                {
                    self.said = true;
                }
            }
            if !choice["delta"]["tool_calls"].is_null()
                || !choice["delta"]["function_call"].is_null()
            {
                let delta = choice["delta"]
                    .as_object_mut()
                    .ok_or_else(|| invalid("Zen returned an invalid choice delta."))?;
                if !self.pending.contains_key(&index) {
                    let metadata_bytes = serde_json::to_vec(&self.metadata).unwrap().len();
                    self.bytes = self.bytes.saturating_add(metadata_bytes);
                    self.pending.insert(
                        index,
                        Calls {
                            metadata: self.metadata.clone(),
                            bytes: metadata_bytes,
                            ..Calls::default()
                        },
                    );
                }
                let state = self.pending.get_mut(&index).unwrap();
                let charge = serde_json::to_vec(delta).unwrap().len();
                self.bytes = self.bytes.saturating_add(charge);
                state.bytes = state.bytes.saturating_add(charge);
                if self.bytes > MAX_PENDING {
                    return Err(invalid("Zen's pending tools exceed 32 MiB."));
                }
                if let Some(calls) = delta.remove("tool_calls") {
                    for fragment in array(&calls)? {
                        if !fragment.is_object()
                            || (!fragment["type"].is_null() && fragment["type"] != "function")
                        {
                            return Err(invalid("Zen returned an unsupported tool delta."));
                        }
                        let position = position(&fragment["index"])?;
                        let call = state
                            .calls
                            .entry(position)
                            .or_insert_with(|| json!({"index":position,"function":{}}));
                        for (key, value) in fragment.as_object().unwrap() {
                            if key == "function" {
                                if !value.is_null() {
                                    append_function(&mut call["function"], value)?;
                                }
                            } else if key != "id"
                                || value.as_str().is_some_and(|s| !s.is_empty())
                                || call["id"].as_str().is_none_or(str::is_empty)
                            {
                                call[key] = value.clone();
                            }
                        }
                    }
                }
                if let Some(function) = delta.remove("function_call") {
                    append_function(&mut state.legacy, &function)?;
                }
            }
            if !choice["finish_reason"].is_null() {
                let delta = self.complete(index, names)?;
                if let Some(own) = choice["delta"].as_object_mut() {
                    for (key, value) in delta.as_object().unwrap() {
                        let merged = if key == "content" {
                            own.get(key).and_then(Value::as_str).map(|previous| {
                                json!(format!("{previous}{}", value.as_str().unwrap_or("")))
                            })
                        } else {
                            None
                        };
                        own.insert(key.clone(), merged.unwrap_or_else(|| value.clone()));
                    }
                } else {
                    choice["delta"] = delta;
                }
                if self.dropped.contains(&index)
                    && [Some("tool_calls"), Some("function_call")]
                        .contains(&choice["finish_reason"].as_str())
                {
                    choice["finish_reason"] = json!("stop");
                }
                self.finished.insert(index);
                self.open.remove(&index);
            } else if !self.finished.contains(&index) {
                self.open.insert(index);
            }
        }
        choices.retain(|choice| {
            !choice["finish_reason"].is_null()
                || choice["delta"]
                    .as_object()
                    .is_some_and(|delta| !delta.is_empty())
                || choice.as_object().is_some_and(|map| {
                    map.keys()
                        .any(|key| !["index", "delta", "finish_reason"].contains(&key.as_str()))
                })
        });
        Ok(if choices.is_empty() && data["usage"].is_null() {
            None
        } else {
            Some(data)
        })
    }
    fn finish(&mut self, names: &Names) -> PluginResult<Option<Value>> {
        if self.pending.is_empty() {
            return Ok(None);
        }
        let mut data = self.pending.values().next().unwrap().metadata.clone();
        let mut choices = Vec::new();
        for index in self.pending.keys().copied().collect::<Vec<_>>() {
            choices.push(
                json!({"index":index,"delta":self.complete(index,names)?,"finish_reason":null}),
            );
        }
        data["choices"] = json!(choices);
        Ok(Some(data))
    }
    fn cut(&self) -> Option<Value> {
        if !self.said || !self.pending.is_empty() {
            return None;
        }
        let mut data = self.metadata.clone();
        data["object"] = json!("chat.completion.chunk");
        data["choices"]=json!(self.open.iter().map(|index|json!({"index":index,"delta":{"content":format!("{}(The reply was cut short: Zen closed the stream before it finished. Ask again to go on.)",if self.wrote.contains(index){"\n\n"}else{""})},"finish_reason":"stop"})).collect::<Vec<_>>());
        Some(data)
    }
}
pub(crate) fn error(protocol: Protocol, message: &str) -> Value {
    if protocol == Protocol::Anthropic {
        json!({"type":"error","error":{"type":"api_error","message":message}})
    } else if protocol == Protocol::Responses {
        json!({"type":"error","code":"upstream_error","message":message})
    } else {
        json!({"error":{"type":"upstream_error","message":message}})
    }
}
struct Guard {
    frames: BoxStream<'static, PluginResult<Frame>>,
    protocol: Protocol,
    names: Names,
    chat: Chat,
    left: BTreeSet<u64>,
    left_ids: BTreeSet<String>,
    tracked_bytes: usize,
    kept: usize,
    dropped: usize,
    completed: bool,
    stopped: bool,
    queue: VecDeque<Frame>,
    streaming: bool,
}
impl Guard {
    fn stop(&mut self) {
        self.stopped = true;
        self.frames = Box::pin(futures::stream::empty());
    }

    fn apply(&mut self, frame: Frame) -> PluginResult<()> {
        if frame.name != "error" && frame.text.trim().is_empty() {
            self.queue.push_back(frame);
            return Ok(());
        }
        if frame.text.trim() == "[DONE]" && frame.name != "error" {
            if self.protocol != Protocol::Chat {
                return Err(invalid("Zen returned an unexpected completion marker."));
            }
            if let Some(value) = self.chat.finish(&self.names)? {
                self.queue.push_back(Frame::json(value, ""));
            }
            self.completed = true;
            self.queue.push_back(frame);
            return Ok(());
        }
        let mut data = if frame.name == "error" {
            return Err(invalid("Zen returned an upstream error event."));
        } else {
            frame.value()?
        };
        let kind = data["type"].as_str().unwrap_or(&frame.name).to_owned();
        if kind == "error"
            || !data["error"].is_null()
            || kind == "response.failed"
            || data["response"]["status"] == "failed"
        {
            self.stop();
            self.queue.push_back(frame);
            return Ok(());
        }
        if self.protocol == Protocol::Chat {
            if let Some(value) = self.chat.apply(data, &self.names)? {
                self.queue.push_back(frame.rewrite(value));
            }
            return Ok(());
        }
        if self.protocol == Protocol::Responses {
            if kind == "response.output_item.added"
                && data["item"]["type"] == "function_call"
                && self.names.check(&data["item"]["name"]).is_none()
            {
                let index = position(&data["output_index"])?;
                let item = replacement(self.protocol, &data["item"]);
                let id = item["id"].as_str().unwrap().to_owned();
                self.left.insert(index);
                for tracked_id in [data["item"]["id"].as_str(), Some(id.as_str())]
                    .into_iter()
                    .flatten()
                {
                    if self.left_ids.insert(tracked_id.to_owned()) {
                        self.tracked_bytes = self.tracked_bytes.saturating_add(tracked_id.len());
                        if self.tracked_bytes > MAX_PENDING {
                            return Err(invalid("Zen's pending tool identifiers exceed 32 MiB."));
                        }
                    }
                }
                data["item"] = item;
                data["item"]["status"] = json!("in_progress");
                data["item"]["content"] = json!([]);
                self.queue.push_back(frame.rewrite(data));
                let part = json!({"type":"output_text","text":"","annotations":[]});
                for (kind, extra) in [
                    ("response.content_part.added", json!({"part":part})),
                    ("response.output_text.delta", json!({"delta":NOTE})),
                    ("response.output_text.done", json!({"text":NOTE})),
                    (
                        "response.content_part.done",
                        json!({"part":{"type":"output_text","text":NOTE,"annotations":[]}}),
                    ),
                ] {
                    let mut value =
                        json!({"type":kind,"item_id":id,"output_index":index,"content_index":0});
                    value
                        .as_object_mut()
                        .unwrap()
                        .extend(extra.as_object().unwrap().clone());
                    self.queue.push_back(Frame::json(value, kind));
                }
                return Ok(());
            }
            if kind.starts_with("response.function_call_arguments.")
                && (data["output_index"]
                    .as_u64()
                    .is_some_and(|index| self.left.contains(&index))
                    || data["item_id"]
                        .as_str()
                        .is_some_and(|id| self.left_ids.contains(id)))
            {
                return Ok(());
            }
        } else {
            if kind == "message_start" {
                self.left.clear();
                self.kept = 0;
                self.dropped = 0;
            }
            if kind == "content_block_start"
                && [Some("tool_use"), Some("server_tool_use")]
                    .contains(&data["content_block"]["type"].as_str())
                && self.names.check(&data["content_block"]["name"]).is_none()
            {
                let index = position(&data["index"])?;
                self.left.insert(index);
                self.dropped += 1;
                data["content_block"] = json!({"type":"text","text":""});
                self.queue.push_back(frame.rewrite(data));
                self.queue.push_back(Frame::json(json!({"type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":NOTE}}),"content_block_delta"));
                return Ok(());
            }
            if kind == "content_block_delta" && self.left.contains(&position(&data["index"])?) {
                return Ok(());
            }
            if kind == "message_delta"
                && data["delta"]["stop_reason"] == "tool_use"
                && self.dropped > 0
                && self.kept == 0
            {
                data["delta"]["stop_reason"] = json!("end_turn");
            }
        }
        let (kept, dropped) = snapshot(&mut data, self.protocol, &self.names)?;
        self.kept += kept;
        self.dropped += dropped;
        if (self.protocol == Protocol::Responses
            && ["response.completed", "response.incomplete"].contains(&kind.as_str()))
            || (self.protocol == Protocol::Anthropic && kind == "message_stop")
        {
            self.completed = true;
        }
        self.queue.push_back(frame.rewrite(data));
        Ok(())
    }
}
pub(crate) fn guard(
    body: nora_ai::HttpByteStream,
    protocol: Protocol,
    names: Names,
    streaming: bool,
) -> BoxStream<'static, PluginResult<Frame>> {
    let state = Guard {
        frames: sse::frames(body),
        protocol,
        names,
        chat: Chat::default(),
        left: BTreeSet::new(),
        left_ids: BTreeSet::new(),
        tracked_bytes: 0,
        kept: 0,
        dropped: 0,
        completed: false,
        stopped: false,
        queue: VecDeque::new(),
        streaming,
    };
    Box::pin(futures::stream::unfold(state, |mut state| async move {
        loop {
            if let Some(frame) = state.queue.pop_front() {
                return Some((Ok(frame), state));
            }
            if state.stopped {
                return None;
            }
            match state.frames.next().await {
                Some(Ok(frame)) => {
                    if let Err(error) = state.apply(frame) {
                        state.stop();
                        return Some((Err(error), state));
                    }
                }
                Some(Err(error)) => {
                    state.stop();
                    return Some((Err(error), state));
                }
                None => {
                    if !state.completed && state.protocol == Protocol::Chat && state.streaming {
                        if let Some(value) = state.chat.cut() {
                            state.queue.push_back(Frame::json(value, ""));
                            state.queue.push_back(Frame {
                                name: String::new(),
                                text: "[DONE]".into(),
                                lines: vec!["data: [DONE]".into()],
                            });
                            state.stop();
                            continue;
                        }
                    }
                    state.stop();
                    if !state.completed || !state.chat.pending.is_empty() {
                        return Some((
                            Err(invalid("Zen's SSE ended without a completion marker.")),
                            state,
                        ));
                    }
                    return None;
                }
            }
        }
    }))
}
#[cfg(test)]
#[path = "../test/tools.test.rs"]
mod tests;
