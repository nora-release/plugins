use super::{network, request::Wire, whole};
use futures::StreamExt;
use nora_ai::{HttpByteStream, HttpStreamingResponse};
use nora_provider_plugin_sdk::{FetchResponse, PluginError, PluginResult};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::Duration,
};
enum Line {
    Event(Value),
    Ended,
    Failed(u16),
}
struct Reader {
    body: HttpByteStream,
    buffer: Vec<u8>,
    wrapped: bool,
    eof: bool,
    deadline: tokio::time::Instant,
}
impl Reader {
    async fn next(&mut self) -> PluginResult<Option<Line>> {
        loop {
            let split = self.buffer.iter().position(|b| *b == b'\n');
            if split.is_some() || self.eof && !self.buffer.is_empty() {
                let n = split.unwrap_or(self.buffer.len());
                let bytes = self
                    .buffer
                    .drain(..n + usize::from(split.is_some()))
                    .collect::<Vec<_>>();
                let bytes = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
                if bytes.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                let value: Value = serde_json::from_slice(bytes).map_err(|_| {
                    PluginError::transport("Zed returned an unreadable stream line.")
                })?;
                if !self.wrapped {
                    return Ok(Some(Line::Event(value)));
                }
                if let Some(event) = value.get("event") {
                    return Ok(Some(Line::Event(event.clone())));
                }
                if value["status"] == "stream_ended" {
                    return Ok(Some(Line::Ended));
                }
                if let Some(failed) = value.pointer("/status/failed") {
                    return Ok(Some(Line::Failed(failed_status(
                        failed["code"].as_str().unwrap_or_default(),
                    ))));
                }
                continue;
            }
            if self.eof {
                return Ok(None);
            }
            let limit = self
                .deadline
                .min(tokio::time::Instant::now() + Duration::from_secs(120));
            match tokio::time::timeout_at(limit, self.body.next())
                .await
                .map_err(|_| PluginError::transport("Zed reply timed out."))?
            {
                Some(Ok(chunk)) => {
                    if self.buffer.len().saturating_add(chunk.len()) > 8 * 1024 * 1024 {
                        return Err(PluginError::transport(
                            "Zed returned an oversized stream line.",
                        ));
                    }
                    self.buffer.extend_from_slice(&chunk);
                }
                Some(Err(_)) => {
                    return Err(PluginError::transport(
                        "Zed reply broke off before completion.",
                    ))
                }
                None => self.eof = true,
            }
        }
    }
}
pub(crate) fn failed_status(code: &str) -> u16 {
    for prefix in ["upstream_http_", "http_"] {
        if let Some(n) = code
            .strip_prefix(prefix)
            .and_then(|s| s.parse::<u16>().ok())
            .filter(|s| (400..600).contains(s))
        {
            return n;
        }
    }
    if code.contains("rate_limit") {
        429
    } else if code.contains("overloaded") {
        529
    } else if code.contains("billing") || code.contains("payment") {
        402
    } else if code.contains("context_length") {
        400
    } else {
        502
    }
}
fn event_error(wire: Wire, event: &Value) -> Option<u16> {
    if !event.is_object() {
        return Some(502);
    }
    if event.get("error").is_some_and(|e| !e.is_null()) {
        let error = &event["error"];
        return Some(
            error["code"]
                .as_u64()
                .filter(|s| (400..600).contains(s))
                .map(|s| s as u16)
                .unwrap_or(match error["type"].as_str() {
                    Some("rate_limit_error") => 429,
                    Some("authentication_error") => 401,
                    Some("permission_error") => 403,
                    Some("overloaded_error") => 529,
                    _ => 502,
                }),
        );
    }
    if wire == Wire::Responses
        && (event["type"] == "error"
            || event["type"] == "response.failed"
            || event
                .pointer("/response/error")
                .is_some_and(|e| !e.is_null()))
    {
        return Some(502);
    }
    None
}
pub(crate) fn error_body(wire: Wire, status: u16) -> Value {
    let message = network::failure(status).message;
    match wire {
        Wire::Anthropic => {
            json!({"type":"error","error":{"type":match status{400=>"invalid_request_error",401=>"authentication_error",402=>"billing_error",403=>"permission_error",404=>"not_found_error",429=>"rate_limit_error",529=>"overloaded_error",_=>"api_error"},"message":message}})
        }
        Wire::Responses => {
            json!({"type":"error","code":status.to_string(),"message":message,"error":{"message":message,"code":status.to_string()}})
        }
        Wire::Gemini => {
            json!({"error":{"code":status,"message":message,"status":match status{400=>"INVALID_ARGUMENT",401=>"UNAUTHENTICATED",402|429=>"RESOURCE_EXHAUSTED",403=>"PERMISSION_DENIED",404=>"NOT_FOUND",529=>"UNAVAILABLE",_=>"INTERNAL"}}})
        }
        Wire::Chat => json!({"error":{"message":message,"type":"api_error","code":status}}),
    }
}
pub(crate) fn failure(wire: Wire, status: u16) -> FetchResponse {
    FetchResponse::bytes(
        status,
        BTreeMap::from([("content-type".into(), "application/json".into())]),
        serde_json::to_vec(&error_body(wire, status)).unwrap(),
    )
}
fn sse(wire: Wire, event: &Value) -> PluginResult<Vec<u8>> {
    let mut output = String::new();
    if matches!(wire, Wire::Anthropic | Wire::Responses) {
        if let Some(kind) = event["type"].as_str() {
            if !kind
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            {
                return Err(PluginError::transport(
                    "Zed returned an invalid event type.",
                ));
            }
            output.push_str(&format!("event: {kind}\n"));
        }
    }
    output.push_str(&format!("data: {event}\n\n"));
    Ok(output.into_bytes())
}
fn said(wire: Wire, event: &Value) -> bool {
    match wire {
        Wire::Anthropic => match event["type"].as_str() {
            Some("message_start" | "ping" | "content_block_stop") => false,
            Some("content_block_start") => {
                let block = &event["content_block"];
                !["text", "thinking", "redacted_thinking"]
                    .contains(&block["type"].as_str().unwrap_or_default())
                    || block["text"].as_str().is_some_and(|s| !s.is_empty())
                    || block["thinking"].as_str().is_some_and(|s| !s.is_empty())
            }
            Some("content_block_delta") => ["text", "thinking", "partial_json"]
                .into_iter()
                .any(|k| event["delta"][k].as_str().is_some_and(|s| !s.is_empty())),
            _ => true,
        },
        Wire::Responses => match event["type"].as_str() {
            Some(
                "response.created"
                | "response.in_progress"
                | "response.queued"
                | "response.content_part.added"
                | "response.reasoning_summary_part.added",
            ) => false,
            Some("response.output_item.added") => !["message", "reasoning"]
                .contains(&event["item"]["type"].as_str().unwrap_or_default()),
            Some(
                "response.output_text.delta"
                | "response.reasoning_summary_text.delta"
                | "response.reasoning_text.delta",
            ) => event["delta"].as_str().is_some_and(|s| !s.is_empty()),
            _ => true,
        },
        Wire::Chat => event["choices"].as_array().is_some_and(|choices| {
            choices.iter().any(|c| {
                c["finish_reason"].is_string()
                    || ["content", "reasoning_content", "reasoning"]
                        .into_iter()
                        .any(|k| c["delta"][k].as_str().is_some_and(|s| !s.is_empty()))
                    || c["delta"]["tool_calls"]
                        .as_array()
                        .is_some_and(|a| !a.is_empty())
            })
        }),
        Wire::Gemini => event["candidates"].as_array().is_some_and(|c| {
            c.iter().any(|c| {
                c["finishReason"].is_string()
                    || c["content"]["parts"].as_array().is_some_and(|p| {
                        p.iter().any(|p| {
                            p["text"].as_str().is_some_and(|s| !s.is_empty())
                                || p.get("functionCall").is_some()
                                || p["thought"] == true
                        })
                    })
            })
        }),
    }
}
struct State {
    reader: Reader,
    wire: Wire,
    queue: VecDeque<Vec<u8>>,
    ended: bool,
    done: bool,
    terminal: bool,
    choices: BTreeSet<u64>,
    finished: BTreeSet<u64>,
    tool_choices: BTreeSet<u64>,
    text_choices: BTreeSet<u64>,
    meta: Value,
}
impl State {
    fn observe(&mut self, event: &Value) {
        match self.wire {
            Wire::Anthropic => {
                self.terminal |= event["type"] == "message_stop"
                    || event["type"] == "message_delta" && event["delta"]["stop_reason"].is_string()
            }
            Wire::Responses => {
                self.terminal |= ["response.completed", "response.incomplete"]
                    .contains(&event["type"].as_str().unwrap_or_default())
            }
            Wire::Gemini => {
                self.terminal |= event["candidates"]
                    .as_array()
                    .is_some_and(|c| c.iter().any(|c| c["finishReason"].is_string()))
            }
            Wire::Chat => {
                if self.meta.is_null() {
                    self.meta =
                        json!({"id":event["id"],"created":event["created"],"model":event["model"]});
                }
                for c in event["choices"].as_array().into_iter().flatten() {
                    let i = c["index"].as_u64().unwrap_or(0);
                    self.choices.insert(i);
                    if c["finish_reason"].is_string() {
                        self.finished.insert(i);
                    }
                    if c["delta"]["tool_calls"]
                        .as_array()
                        .is_some_and(|c| !c.is_empty())
                    {
                        self.tool_choices.insert(i);
                    }
                    if c["delta"]["content"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty())
                    {
                        self.text_choices.insert(i);
                    }
                }
                self.terminal = !self.choices.is_empty() && self.choices.is_subset(&self.finished);
            }
        }
    }
    fn error(&mut self, status: u16) -> PluginResult<Option<Vec<u8>>> {
        self.done = true;
        Ok(Some(sse(self.wire, &error_body(self.wire, status))?))
    }
    async fn next(&mut self) -> PluginResult<Option<Vec<u8>>> {
        if let Some(bytes) = self.queue.pop_front() {
            return Ok(Some(bytes));
        }
        if self.done {
            return Ok(None);
        }
        loop {
            match self.reader.next().await {
                Ok(Some(Line::Ended)) => {
                    self.ended = true;
                    return self.end();
                }
                Ok(Some(Line::Failed(status))) => return self.error(status),
                Ok(Some(Line::Event(event))) => {
                    if let Some(status) = event_error(self.wire, &event) {
                        return self.error(status);
                    }
                    self.observe(&event);
                    return Ok(Some(sse(self.wire, &event)?));
                }
                Ok(None) => return self.end(),
                Err(_) => return self.error(502),
            }
        }
    }
    fn end(&mut self) -> PluginResult<Option<Vec<u8>>> {
        if self.wire == Wire::Chat
            && !self.terminal
            && !self.ended
            && self.reader.wrapped
            && self.tool_choices.is_empty()
            && !self.text_choices.is_empty()
        {
            let choices:Vec<_>=self.choices.difference(&self.finished).map(|i|json!({"index":i,"delta":{"content":"\n\n(The reply was cut short: Zed closed the stream before it finished. Ask again to go on.)"},"finish_reason":null})).collect();
            let mut note = self.meta.clone();
            note["object"] = json!("chat.completion.chunk");
            note["choices"] = json!(choices);
            self.queue.push_back(sse(self.wire, &note)?);
            note["choices"] = json!(self
                .choices
                .difference(&self.finished)
                .map(|i| json!({"index":i,"delta":{},"finish_reason":"stop"}))
                .collect::<Vec<_>>());
            self.queue.push_back(sse(self.wire, &note)?);
            self.queue.push_back(b"data: [DONE]\n\n".to_vec());
            self.done = true;
            return Ok(self.queue.pop_front());
        }
        if !self.terminal || self.reader.wrapped && !self.ended && self.wire != Wire::Chat {
            return self.error(502);
        }
        self.done = true;
        if self.wire == Wire::Chat {
            Ok(Some(b"data: [DONE]\n\n".to_vec()))
        } else {
            Ok(None)
        }
    }
}
pub(crate) async fn response(
    wire: Wire,
    streaming: bool,
    response: HttpStreamingResponse,
) -> PluginResult<FetchResponse> {
    let wrapped = network::header(&response, "x-zed-server-supports-status-messages").is_some();
    let reader = Reader {
        body: response.body,
        buffer: Vec::new(),
        wrapped,
        eof: false,
        deadline: tokio::time::Instant::now() + Duration::from_secs(15 * 60),
    };
    let mut state = State {
        reader,
        wire,
        queue: VecDeque::new(),
        ended: false,
        done: false,
        terminal: false,
        choices: BTreeSet::new(),
        finished: BTreeSet::new(),
        tool_choices: BTreeSet::new(),
        text_choices: BTreeSet::new(),
        meta: Value::Null,
    };
    if !streaming {
        let mut events = Vec::new();
        let mut size = 0;
        while let Some(line) = state.reader.next().await? {
            match line {
                Line::Event(event) => {
                    if let Some(status) = event_error(wire, &event) {
                        return Ok(failure(wire, status));
                    }
                    size += event.to_string().len();
                    if size > 32 * 1024 * 1024 || events.len() > 100_000 {
                        return Err(PluginError::transport("The Zed reply is too large."));
                    }
                    events.push(event);
                }
                Line::Failed(status) => return Ok(failure(wire, status)),
                Line::Ended => {
                    state.ended = true;
                    break;
                }
            }
        }
        if wrapped && !state.ended {
            return Ok(failure(wire, 502));
        }
        let value = whole::assemble(wire, &events)?;
        return Ok(FetchResponse::bytes(
            200,
            BTreeMap::from([("content-type".into(), "application/json".into())]),
            serde_json::to_vec(&value).unwrap(),
        ));
    }
    let mut size = 0;
    let mut began = false;
    while let Some(line) = state.reader.next().await? {
        match line {
            Line::Event(event) => {
                if let Some(status) = event_error(wire, &event) {
                    return Ok(failure(wire, status));
                }
                size += event.to_string().len();
                if size > 1024 * 1024 {
                    return Ok(failure(wire, 502));
                }
                state.observe(&event);
                state.queue.push_back(sse(wire, &event)?);
                if said(wire, &event) {
                    began = true;
                    break;
                }
            }
            Line::Failed(status) => return Ok(failure(wire, status)),
            Line::Ended => {
                state.ended = true;
                break;
            }
        }
    }
    if !began {
        return Ok(failure(wire, 502));
    }
    let body = futures::stream::unfold(Some(state), |state| async move {
        let mut state = state?;
        match state.next().await {
            Ok(Some(bytes)) => Some((Ok(bytes), Some(state))),
            Ok(None) => None,
            Err(error) => Some((Err(error), None)),
        }
    });
    Ok(FetchResponse {
        status: 200,
        headers: BTreeMap::from([
            ("content-type".into(), "text/event-stream".into()),
            ("cache-control".into(), "no-cache".into()),
        ]),
        body: Box::pin(body),
    })
}
#[cfg(test)]
#[path = "../test/stream.test.rs"]
mod tests;
