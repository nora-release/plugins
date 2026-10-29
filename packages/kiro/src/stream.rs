use super::request::Names;
use futures::{stream, StreamExt};
use nora_ai::{HttpByteStream, HttpStreamingResponse};
use nora_provider_plugin_sdk::{FetchResponse, PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
fn broken() -> PluginError {
    PluginError::transport("The Kiro event stream is invalid or incomplete.")
}
fn error_status(kind: &str, text: &str) -> u16 {
    let combined = format!("{kind} {text}").to_ascii_lowercase();
    if combined.contains("content_length_exceeds_threshold")
        || combined.contains("input is too long")
    {
        400
    } else if combined.contains("insufficient_model_capacity") || combined.contains("unavailable") {
        503
    } else if [
        "throttl",
        "quota",
        "insufficient",
        "balance",
        "credit",
        "billing",
        "exceeded",
        "rate limit",
        "rate_limit",
        "usage_limit",
        "monthly_request_count",
        "too many requests",
        "overload",
    ]
    .iter()
    .any(|word| combined.contains(word))
    {
        429
    } else {
        502
    }
}
pub(crate) fn failure(status: u16, text: &str) -> FetchResponse {
    let classified = error_status("", text);
    let status = if classified != 502 {
        classified
    } else {
        status
    };
    let kind = match status {
        400 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        429 => "rate_limit_error",
        503 => "overloaded_error",
        _ => "api_error",
    };
    let message = match status {
        400 => "The input is too long or invalid for Kiro.",
        401 | 403 => "Kiro did not accept this account's credentials.",
        429 => "Kiro usage or rate limit reached.",
        503 => "Kiro has insufficient model capacity.",
        _ => "The Kiro response was invalid or incomplete.",
    };
    FetchResponse::bytes(
        status,
        BTreeMap::from([("content-type".into(), "application/json".into())]),
        serde_json::to_vec(&json!({"type":"error","error":{"type":kind,"message":message}}))
            .unwrap(),
    )
}
fn frame(bytes: &[u8]) -> PluginResult<(BTreeMap<String, String>, Value)> {
    if bytes.len() < 16 {
        return Err(broken());
    }
    let total = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
    let headers = u32::from_be_bytes(bytes[4..8].try_into().unwrap()) as usize;
    if total != bytes.len()
        || headers > total - 16
        || crc32fast::hash(&bytes[..8]) != u32::from_be_bytes(bytes[8..12].try_into().unwrap())
        || crc32fast::hash(&bytes[..total - 4])
            != u32::from_be_bytes(bytes[total - 4..].try_into().unwrap())
    {
        return Err(broken());
    }
    let mut out = BTreeMap::new();
    let mut cursor = 12;
    let end = 12 + headers;
    while cursor < end {
        let size = bytes[cursor] as usize;
        cursor += 1;
        if size == 0 || cursor + size + 1 > end {
            return Err(broken());
        }
        let name = std::str::from_utf8(&bytes[cursor..cursor + size])
            .map_err(|_| broken())?
            .to_owned();
        cursor += size;
        let kind = bytes[cursor];
        cursor += 1;
        let size = match kind {
            0 | 1 => 0,
            2 => 1,
            3 => 2,
            4 => 4,
            5 | 8 => 8,
            9 => 16,
            6 | 7 => {
                if cursor + 2 > end {
                    return Err(broken());
                }
                let size =
                    u16::from_be_bytes(bytes[cursor..cursor + 2].try_into().unwrap()) as usize;
                cursor += 2;
                size
            }
            _ => return Err(broken()),
        };
        if cursor + size > end {
            return Err(broken());
        }
        if kind == 7 {
            let value = std::str::from_utf8(&bytes[cursor..cursor + size]).map_err(|_| broken())?;
            if out.insert(name, value.into()).is_some() {
                return Err(broken());
            }
        }
        cursor += size;
    }
    let payload = serde_json::from_slice(&bytes[end..total - 4]).map_err(|_| broken())?;
    Ok((out, payload))
}
#[derive(Default)]
struct Think {
    state: u8,
    buffer: String,
}
impl Think {
    fn feed(&mut self, text: &str) -> Vec<(bool, String)> {
        self.buffer.push_str(text);
        let mut out = Vec::new();
        loop {
            if self.state == 0 {
                let trimmed = self.buffer.trim_start();
                if let Some(rest) = trimmed.strip_prefix("<thinking>") {
                    self.buffer = rest.trim_start_matches(['\r', '\n']).into();
                    self.state = 1;
                    continue;
                }
                if "<thinking>".starts_with(trimmed) {
                    break;
                }
                self.state = 2;
            }
            if self.state == 1 {
                if let Some(end) = self.buffer.find("</thinking>") {
                    if end > 0 {
                        out.push((true, self.buffer[..end].into()));
                    }
                    self.buffer = self.buffer[end + 11..]
                        .trim_start_matches(['\r', '\n'])
                        .into();
                    self.state = 2;
                    continue;
                }
                let keep = (1..11)
                    .rev()
                    .find(|size| self.buffer.ends_with(&"</thinking>"[..*size]))
                    .unwrap_or(0);
                let end = self.buffer.len() - keep;
                if end > 0 {
                    out.push((true, self.buffer[..end].into()));
                    self.buffer = self.buffer[end..].into();
                }
                break;
            }
            if !self.buffer.is_empty() {
                out.push((false, std::mem::take(&mut self.buffer)));
            }
            break;
        }
        out
    }
    fn finish(&mut self) -> Vec<(bool, String)> {
        if self.buffer.is_empty() {
            vec![]
        } else {
            vec![(self.state == 1, std::mem::take(&mut self.buffer))]
        }
    }
}
#[derive(Default)]
struct Usage {
    input: u64,
    output: u64,
    read: u64,
    write: u64,
}
impl Usage {
    fn value(&self) -> Value {
        json!({"input_tokens":self.input,"output_tokens":self.output,"cache_read_input_tokens":self.read,"cache_creation_input_tokens":self.write})
    }
    fn add(&mut self, value: &Value) {
        let number = |key: &str| value[key].as_u64().unwrap_or(0);
        self.input = self.input.saturating_add(
            value
                .get("uncachedInputTokens")
                .and_then(Value::as_u64)
                .unwrap_or_else(|| number("inputTokens")),
        );
        self.output = self.output.saturating_add(number("outputTokens"));
        self.read = self.read.saturating_add(number("cacheReadInputTokens"));
        self.write = self.write.saturating_add(number("cacheWriteInputTokens"));
    }
}
struct State {
    body: HttpByteStream,
    buffer: Vec<u8>,
    queued: VecDeque<Vec<u8>>,
    names: Names,
    model: String,
    id: String,
    started: bool,
    done: bool,
    failed: Option<u16>,
    seen_frame: bool,
    kind: String,
    tool: String,
    content: Vec<Value>,
    usage: Usage,
    stop: String,
    tools: u64,
    said: u64,
    pct: f64,
    context: u64,
    think: Think,
    thinking: bool,
    stream: bool,
}
impl State {
    fn send(&mut self, event: &str, value: Value) {
        if self.stream {
            self.queued
                .push_back(format!("event: {event}\ndata: {value}\n\n").into_bytes());
        }
    }
    fn start(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        self.send("message_start",json!({"type":"message_start","message":{"id":self.id,"type":"message","role":"assistant","content":[],"model":self.model,"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":0,"output_tokens":0}}}));
    }
    fn close(&mut self) {
        if self.kind.is_empty() {
            return;
        }
        self.send(
            "content_block_stop",
            json!({"type":"content_block_stop","index":self.content.len()-1}),
        );
        self.kind.clear();
        self.tool.clear();
    }
    fn block(&mut self, kind: &str, tool: Option<(&str, &str)>) {
        if self.content.len() >= 10000 {
            self.fail(502);
            return;
        }
        let new_tool = tool.map(|(id, _)| id).unwrap_or_default();
        if self.kind == kind && (kind != "tool_use" || self.tool == new_tool) {
            return;
        }
        self.close();
        self.start();
        self.kind = kind.into();
        let block = match kind {
            "thinking" => json!({"type":"thinking","thinking":"","signature":""}),
            "tool_use" => {
                let (id, name) = tool.unwrap();
                self.tool = id.into();
                self.tools += 1;
                json!({"type":"tool_use","id":id,"name":self.names.get(name).map(String::as_str).unwrap_or(name),"input":{}})
            }
            _ => json!({"type":"text","text":""}),
        };
        self.content.push(block.clone());
        self.send("content_block_start",json!({"type":"content_block_start","index":self.content.len()-1,"content_block":block}));
        if kind == "tool_use" {
            self.content.last_mut().unwrap()["input"] = json!("");
        }
    }
    fn delta(&mut self, kind: &str, key: &str, text: &str) {
        if text.is_empty() || self.done {
            return;
        }
        let last = self.content.last_mut().unwrap();
        let existing = last[key].as_str().unwrap_or_default();
        if !self.stream || key == "input" {
            if existing.len().saturating_add(text.len()) > 32 * 1024 * 1024 {
                self.fail(502);
                return;
            }
            last[key] = json!(format!("{existing}{text}"));
        }
        let delta = match kind {
            "input_json_delta" => json!({"type":kind,"partial_json":text}),
            "thinking_delta" => json!({"type":kind,"thinking":text}),
            "signature_delta" => json!({"type":kind,"signature":text}),
            _ => json!({"type":kind,"text":text}),
        };
        self.send(
            "content_block_delta",
            json!({"type":"content_block_delta","index":self.content.len()-1,"delta":delta}),
        );
    }
    fn texts(&mut self, pieces: Vec<(bool, String)>) {
        for (thinking, text) in pieces {
            if text.is_empty() {
                continue;
            }
            self.block(if thinking { "thinking" } else { "text" }, None);
            self.delta(
                if thinking {
                    "thinking_delta"
                } else {
                    "text_delta"
                },
                if thinking { "thinking" } else { "text" },
                &text,
            );
        }
    }
    fn fail(&mut self, status: u16) {
        self.done = true;
        self.failed = Some(status);
        if self.started {
            self.send("error",json!({"type":"error","error":{"type":if status==429{"rate_limit_error"}else{"api_error"},"message":if status==429{"Kiro usage or rate limit reached."}else{"The Kiro response was invalid or incomplete."}}}));
        }
    }
    fn consume(&mut self, headers: BTreeMap<String, String>, value: Value) -> PluginResult<()> {
        self.seen_frame = true;
        let kind = headers
            .get(":event-type")
            .map(String::as_str)
            .unwrap_or_default();
        if ["exception", "error"].contains(
            &headers
                .get(":message-type")
                .map(String::as_str)
                .unwrap_or_default(),
        ) || [
            "error",
            "throttlingError",
            "validationError",
            "serviceUnavailableError",
            "internalServerException",
        ]
        .contains(&kind)
        {
            self.fail(error_status(
                headers
                    .get(":exception-type")
                    .or_else(|| headers.get(":error-code"))
                    .map(String::as_str)
                    .unwrap_or(kind),
                &value.to_string(),
            ));
            return Ok(());
        }
        match kind {
            "assistantResponseEvent" => {
                let text = value["content"].as_str().unwrap_or_default();
                self.said = self.said.saturating_add(text.chars().count() as u64);
                if self.thinking {
                    let pieces = self.think.feed(text);
                    self.texts(pieces);
                } else {
                    self.texts(vec![(false, text.into())]);
                }
            }
            "reasoningContentEvent" => {
                if let Some(text) = value["text"].as_str().filter(|text| !text.is_empty()) {
                    self.block("thinking", None);
                    self.delta("thinking_delta", "thinking", text);
                    self.said += text.chars().count() as u64;
                }
                if let Some(signature) = value["signature"]
                    .as_str()
                    .filter(|signature| !signature.is_empty())
                {
                    self.block("thinking", None);
                    self.delta("signature_delta", "signature", signature);
                }
            }
            "toolUseEvent" => {
                if let Some(id) = value["toolUseId"].as_str() {
                    if self.tool != id {
                        if id.is_empty() || value["name"].as_str().is_none_or(str::is_empty) {
                            return Err(broken());
                        }
                        let pieces = self.think.finish();
                        self.texts(pieces);
                        self.block(
                            "tool_use",
                            Some((id, value["name"].as_str().unwrap_or_default())),
                        );
                    }
                }
                if let Some(input) = value.get("input").filter(|input| !input.is_null()) {
                    if self.kind != "tool_use" {
                        return Err(broken());
                    }
                    let input = input.as_str().map(str::to_owned).unwrap_or_else(|| {
                        if input == &json!({}) {
                            String::new()
                        } else {
                            input.to_string()
                        }
                    });
                    self.said += input.chars().count() as u64;
                    self.delta("input_json_delta", "input", &input);
                }
            }
            "metadataEvent" | "messageMetadataEvent" => {
                if let Some(stop) = value["stopReason"].as_str() {
                    self.stop = stop.into();
                }
                if let Some(tokens) = value.get("tokenUsage") {
                    self.usage.add(tokens);
                    self.pct = tokens["contextUsagePercentage"]
                        .as_f64()
                        .filter(|pct| pct.is_finite() && *pct > 0.0)
                        .unwrap_or(self.pct);
                }
            }
            "contextUsageEvent" => {
                self.pct = value["contextUsagePercentage"]
                    .as_f64()
                    .filter(|pct| pct.is_finite() && *pct > 0.0)
                    .unwrap_or(self.pct);
            }
            _ => {}
        }
        Ok(())
    }
    fn finish(&mut self) {
        if self.done {
            return;
        }
        if !self.buffer.is_empty() || !self.seen_frame {
            self.fail(502);
            return;
        }
        let pieces = self.think.finish();
        self.texts(pieces);
        self.close();
        self.start();
        if self.usage.input == 0 && self.pct > 0.0 && self.context > 0 {
            self.usage.input = ((self.pct.min(100.0) / 100.0) * self.context as f64) as u64;
        }
        if self.usage.output == 0 {
            self.usage.output = self.said.div_ceil(4);
        }
        let reason = if self.tools > 0 {
            "tool_use"
        } else if self.stop.eq_ignore_ascii_case("max_tokens") {
            "max_tokens"
        } else if self.stop.eq_ignore_ascii_case("content_filtered") {
            "refusal"
        } else {
            "end_turn"
        };
        for block in &mut self.content {
            if block["type"] == "tool_use" {
                let Some(input) = block["input"].as_str() else {
                    self.fail(502);
                    return;
                };
                let parsed = if input.is_empty() {
                    Ok(json!({}))
                } else {
                    serde_json::from_str::<Value>(input)
                };
                match parsed {
                    Ok(value) if value.is_object() => block["input"] = value,
                    _ => {
                        self.fail(502);
                        return;
                    }
                }
            }
        }
        self.send("message_delta",json!({"type":"message_delta","delta":{"stop_reason":reason,"stop_sequence":null},"usage":self.usage.value()}));
        self.send("message_stop", json!({"type":"message_stop"}));
        if !self.stream {
            self.queued.push_back(serde_json::to_vec(&json!({"id":self.id,"type":"message","role":"assistant","model":self.model,"content":self.content,"stop_reason":reason,"stop_sequence":null,"usage":self.usage.value()})).unwrap());
        }
        self.done = true;
    }
    async fn next(&mut self) -> Option<Vec<u8>> {
        loop {
            if let Some(chunk) = self.queued.pop_front() {
                return Some(chunk);
            }
            if self.done {
                return None;
            }
            if self.buffer.len() >= 12 {
                let total = u32::from_be_bytes(self.buffer[..4].try_into().unwrap()) as usize;
                let headers = u32::from_be_bytes(self.buffer[4..8].try_into().unwrap()) as usize;
                if !(16..=16 * 1024 * 1024).contains(&total) || headers > total - 16 {
                    self.fail(502);
                    continue;
                }
                if self.buffer.len() >= total {
                    let bytes = self.buffer.drain(..total).collect::<Vec<_>>();
                    match frame(&bytes).and_then(|(headers, value)| self.consume(headers, value)) {
                        Ok(()) => {}
                        Err(_) => self.fail(502),
                    }
                    continue;
                }
            }
            match tokio::time::timeout(std::time::Duration::from_secs(60), self.body.next()).await {
                Ok(Some(Ok(bytes))) => {
                    self.buffer.extend_from_slice(&bytes);
                    if self.buffer.len() > 32 * 1024 * 1024 {
                        self.fail(502);
                    }
                }
                Ok(None) => self.finish(),
                _ => self.fail(502),
            }
        }
    }
}
pub(crate) async fn response(
    response: HttpStreamingResponse,
    model: String,
    streaming: bool,
    names: Names,
    budget: u64,
    context: u64,
) -> PluginResult<FetchResponse> {
    if !(200..300).contains(&response.status) {
        let status = response.status;
        let text = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            nora_ai::collect_http_body_text_limited(response.body, 64 * 1024),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
        return Ok(failure(status, &text));
    }
    let mut state = State {
        body: response.body,
        buffer: Vec::new(),
        queued: VecDeque::new(),
        names,
        model,
        id: format!("msg_{}", nora_ai::oauth_pkce::random_hex(12)),
        started: false,
        done: false,
        failed: None,
        seen_frame: false,
        kind: String::new(),
        tool: String::new(),
        content: Vec::new(),
        usage: Usage::default(),
        stop: String::new(),
        tools: 0,
        said: 0,
        pct: 0.0,
        context,
        think: Think::default(),
        thinking: budget > 0,
        stream: streaming,
    };
    let first = state.next().await;
    if let Some(status) = state.failed.filter(|_| !state.started || !streaming) {
        return Ok(failure(status, ""));
    }
    let Some(first) = first else {
        return Ok(failure(502, ""));
    };
    let body = stream::once(async move { Ok(first) })
        .chain(stream::unfold(state, |mut state| async move {
            state.next().await.map(|chunk| (Ok(chunk), state))
        }));
    Ok(FetchResponse {
        status: 200,
        headers: BTreeMap::from([(
            "content-type".into(),
            if streaming {
                "text/event-stream".into()
            } else {
                "application/json".into()
            },
        )]),
        body: Box::pin(body),
    })
}
#[cfg(test)]
#[path = "../test/stream.test.rs"]
mod tests;
