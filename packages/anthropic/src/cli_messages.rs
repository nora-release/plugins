use super::{cli, search_results, web_search::sum_usage};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy)]
enum Received {
    Streamed,
    Whole,
}

#[derive(Clone, Default)]
pub(crate) struct History {
    received: BTreeMap<String, Received>,
    searches: search_results::History,
}
impl History {
    fn remember(&mut self, id: &str, source: Received) -> PluginResult<()> {
        if self.received.len() >= 10_000 && !self.received.contains_key(id) {
            return Err(PluginError::transport(
                "The Claude conversation has too many messages.",
            ));
        }
        self.received.insert(id.to_owned(), source);
        Ok(())
    }
}

struct Whole {
    id: String,
    blocks: usize,
    stop: Value,
    usage: Value,
}

/// One CLI reply can contain internal search turns, retried streams and whole
/// message fragments. Only completed whole messages and actual stream IDs are
/// remembered across fetches; pending same-ID fragments remain appendable.
#[derive(Default)]
pub(crate) struct Receiver {
    enabled: bool,
    schema: bool,
    hidden: BTreeSet<u64>,
    history: History,
    whole: Option<Whole>,
    indexes: BTreeMap<u64, usize>,
    own: BTreeSet<u64>,
    next_index: usize,
    client_tools: bool,
    words: bool,
    stop: bool,
    continues: bool,
    before: Value,
    current: Value,
    started: bool,
    pending: Vec<search_results::Source>,
}
impl Receiver {
    pub fn new(search: bool) -> Self {
        Self {
            enabled: search,
            ..Default::default()
        }
    }
    pub fn with_schema(mut self, schema: bool) -> Self {
        self.schema = schema;
        self
    }
    pub fn structured_result(&mut self, value: &Value) -> PluginResult<Vec<Value>> {
        if !self.schema || !self.started || value.is_null() || self.next_index >= 10_000 {
            return Err(PluginError::transport(super::structured_output::MISSING));
        }
        let usage = if self.continues {
            self.before.clone()
        } else {
            self.usage()
        };
        self.continues = false;
        let index = self.next_index;
        self.next_index += 1;
        Ok(vec![
            json!({"type":"content_block_start","index":index,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":value.to_string()}}),
            json!({"type":"content_block_stop","index":index}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":usage}),
        ])
    }
    pub fn restore_history(&mut self, history: History) {
        self.history = history;
    }
    pub fn take_history(&mut self) -> History {
        std::mem::take(&mut self.history)
    }
    pub fn seen(&self, id: &str) -> bool {
        self.history.received.contains_key(id)
    }
    pub fn pending_whole(&self) -> bool {
        self.whole.is_some()
    }
    pub fn should_settle(&self, envelope: &Value) -> bool {
        self.whole.as_ref().is_some_and(|whole| {
            envelope["type"] != "assistant"
                || envelope["message"]["id"] != whole.id
                || envelope.get("error").is_some()
                || envelope["message"].get("error").is_some()
        })
    }
    pub fn finish_whole(&mut self) -> PluginResult<Vec<Value>> {
        let Some(whole) = self.whole.take() else {
            return Ok(Vec::new());
        };
        self.history.remember(&whole.id, Received::Whole)?;
        let mut events = self.normalize(
            json!({"type":"message_delta","delta":{"stop_reason":whole.stop},"usage":whole.usage}),
        )?;
        events.extend(self.normalize(json!({"type":"message_stop"}))?);
        Ok(events)
    }
    pub fn whole(&mut self, message: &Value) -> PluginResult<Vec<Value>> {
        let id = cli::text(message.get("id"), 256).ok_or_else(|| {
            PluginError::transport("Claude Code returned a message without an identifier.")
        })?;
        if self.seen(id) {
            return Ok(Vec::new());
        }
        let mut output = Vec::new();
        if self.whole.is_none() {
            let mut start = message.clone();
            start["content"] = json!([]);
            start["stop_reason"] = Value::Null;
            start["stop_sequence"] = Value::Null;
            output.extend(self.normalize(json!({"type":"message_start","message":start}))?);
            self.whole = Some(Whole {
                id: id.into(),
                blocks: 0,
                stop: Value::Null,
                usage: json!({}),
            });
        }
        let blocks = message["content"].as_array().ok_or_else(|| {
            PluginError::transport("Claude Code returned invalid message blocks.")
        })?;
        let whole = self.whole.as_mut().unwrap();
        if whole.id != id || whole.blocks.saturating_add(blocks.len()) > 10_000 {
            return Err(PluginError::transport(
                "Claude Code returned invalid message fragments.",
            ));
        }
        let first = whole.blocks;
        whole.blocks += blocks.len();
        if !message["stop_reason"].is_null() {
            whole.stop = message["stop_reason"].clone();
        }
        for (key, value) in message["usage"].as_object().into_iter().flatten() {
            whole.usage[key] = value.clone();
        }
        for (index, block) in blocks.iter().enumerate() {
            output.extend(self.normalize(
                json!({"type":"content_block_start","index":first+index,"content_block":block}),
            )?);
            output
                .extend(self.normalize(json!({"type":"content_block_stop","index":first+index}))?);
        }
        Ok(output)
    }
    pub fn push(&mut self, event: Value) -> PluginResult<Vec<Value>> {
        if event["type"] == "message_start" {
            let id = cli::text(event.pointer("/message/id"), 256).ok_or_else(|| {
                PluginError::transport("Claude Code returned a message without an identifier.")
            })?;
            self.history.remember(id, Received::Streamed)?;
        }
        self.normalize(event)
    }
    pub fn result(&mut self, event: &Value) -> PluginResult<Vec<Value>> {
        if !self.enabled {
            return Ok(Vec::new());
        }
        if let Some(source) = self.history.searches.result(event)? {
            self.pending.push(source);
        }
        if self.started {
            self.sources()
        } else {
            Ok(Vec::new())
        }
    }
    fn sources(&mut self) -> PluginResult<Vec<Value>> {
        if self
            .next_index
            .saturating_add(self.pending.len().saturating_mul(2))
            > 10_000
        {
            return Err(PluginError::transport(
                "The Claude reply has too many blocks.",
            ));
        }
        let mut output = Vec::new();
        for source in std::mem::take(&mut self.pending) {
            let index = self.next_index;
            self.next_index += 2;
            output.extend([
                json!({"type":"content_block_start","index":index,"content_block":{"type":"server_tool_use","id":source.id,"name":"web_search","input":{}}}),
                json!({"type":"content_block_delta","index":index,"delta":{"type":"input_json_delta","partial_json":json!({"query":source.query}).to_string()}}),
                json!({"type":"content_block_stop","index":index}),
                json!({"type":"content_block_start","index":index+1,"content_block":{"type":"web_search_tool_result","tool_use_id":source.id,"content":source.pages}}),
                json!({"type":"content_block_stop","index":index+1})
            ]);
        }
        Ok(output)
    }
    fn usage(&self) -> Value {
        sum_usage(&self.before, &self.current)
    }
    fn normalize(&mut self, mut event: Value) -> PluginResult<Vec<Value>> {
        match event["type"].as_str().unwrap_or_default() {
            "message_start" => {
                self.indexes.clear();
                self.own.clear();
                self.hidden.clear();
                self.client_tools = false;
                self.words = false;
                self.stop = false;
                self.current = event["message"]
                    .get("usage")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                self.started = true;
                let first = if self.continues {
                    self.continues = false;
                    json!({"type":"message_delta","delta":{},"usage":self.usage()})
                } else {
                    event
                };
                let mut output = vec![first];
                output.extend(self.sources()?);
                return Ok(output);
            }
            "content_block_start" => {
                let index = block_index(&event)?;
                let block = &event["content_block"];
                if self.schema && block["type"] == "text" {
                    self.hidden.insert(index);
                    return Ok(Vec::new());
                }
                if self.schema && block["type"] == "tool_use" && block["name"] == "StructuredOutput"
                {
                    self.own.insert(index);
                    return Ok(Vec::new());
                }
                if self.enabled && block["type"] == "tool_use" && block["name"] == "WebSearch" {
                    let id = cli::text(block.get("id"), 4096).ok_or_else(|| {
                        PluginError::transport(
                            "Claude Code returned a search call without an identifier.",
                        )
                    })?;
                    self.history.searches.call(id)?;
                    self.own.insert(index);
                    return Ok(Vec::new());
                }
                if block["type"] == "tool_use" {
                    self.client_tools = true;
                }
                if block["type"] == "text"
                    && block["text"].as_str().is_some_and(|text| !text.is_empty())
                {
                    self.words = true;
                }
                if self.next_index >= 10_000
                    || self.indexes.insert(index, self.next_index).is_some()
                {
                    return Err(PluginError::transport("Claude Code reused a reply block."));
                }
                event["index"] = json!(self.next_index);
                self.next_index += 1;
            }
            "content_block_delta" | "content_block_stop" => {
                let index = block_index(&event)?;
                if self.own.contains(&index) || self.hidden.contains(&index) {
                    return Ok(Vec::new());
                }
                if event.pointer("/delta/type").and_then(Value::as_str) == Some("text_delta")
                    && event["delta"]["text"]
                        .as_str()
                        .is_some_and(|text| !text.is_empty())
                {
                    self.words = true;
                }
                event["index"] =
                    json!(self
                        .indexes
                        .get(&index)
                        .ok_or_else(|| PluginError::transport(
                            "Claude Code returned a delta without a block."
                        ))?);
            }
            "message_delta" => {
                for (key, value) in event["usage"].as_object().into_iter().flatten() {
                    if !self.current.is_object() {
                        self.current = json!({});
                    }
                    self.current[key] = value.clone();
                }
                event["usage"] = self.usage();
                if event["delta"]["stop_reason"].as_str().is_some() {
                    self.stop = true;
                }
                if !self.client_tools
                    && (self.schema && self.stop
                        || event["delta"]["stop_reason"] == "tool_use" && !self.own.is_empty())
                {
                    self.continues = true;
                    event["delta"] = json!({});
                }
            }
            "message_stop" => {
                if !self.stop {
                    if self.words || self.client_tools {
                        return Err(PluginError::transport(
                            "Claude Code cut the reply short after showing content.",
                        ));
                    }
                    self.continues = true;
                }
                if self.continues {
                    self.before = self.usage();
                    return Ok(Vec::new());
                }
            }
            _ => {}
        }
        Ok(vec![event])
    }
}
fn block_index(event: &Value) -> PluginResult<u64> {
    event["index"]
        .as_u64()
        .filter(|index| *index < 10_000)
        .ok_or_else(|| PluginError::transport("Claude Code returned an invalid block index."))
}

#[cfg(test)]
#[path = "../test/cli_messages.test.rs"]
mod tests;
