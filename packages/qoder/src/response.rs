use crate::*;
use futures::StreamExt;
use std::collections::VecDeque;
#[path = "splitter.rs"]
mod splitter;
use splitter::{Piece, Splitter};
#[derive(Clone, Debug)]
pub(crate) struct Failure {
    pub status: u16,
    pub message: String,
    pub retry_after: Option<u64>,
    pub sign_in: Option<String>,
    pub queue: Option<queue::State>,
}
impl Failure {
    fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            retry_after: None,
            sign_in: None,
            queue: None,
        }
    }
    pub fn response(self) -> FetchResponse {
        let mut headers = BTreeMap::from([("content-type".into(), "application/json".into())]);
        if let Some(sign_in) = self
            .sign_in
            .or_else(|| (self.status == 401).then(|| "kept".into()))
        {
            if !sign_in.is_empty() {
                headers.insert("x-magpie-sign-in".into(), sign_in);
            }
        }
        if let Some(after) = self.retry_after {
            headers.insert("retry-after".into(), after.to_string());
        }
        FetchResponse::bytes(
            self.status,
            headers,
            json!({"error":{"message":self.message,"type":"qoder_error","code":self.status}})
                .to_string()
                .into_bytes(),
        )
    }
}
pub(crate) fn failure(status: u16, raw: &str) -> Failure {
    let status = if (400..600).contains(&status) {
        status
    } else {
        502
    };
    let mut message = raw.trim().to_owned();
    let mut code = String::new();
    let mut queue = None;
    if let Ok(mut value) = serde_json::from_str::<Value>(raw) {
        let outer = value
            .get("code")
            .filter(|value| !value.is_null())
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string())
            })
            .unwrap_or_default();
        if ["", "401", "403"].contains(&outer.as_str()) {
            if let Ok(inner) = serde_json::from_str::<Value>(text(&value, "message")) {
                if inner
                    .get("code")
                    .is_some_and(|code| code == "10605" || code == 10605)
                {
                    value = inner;
                }
            }
        }
        code = value
            .get("code")
            .filter(|value| !value.is_null())
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string())
            })
            .unwrap_or_default();
        if !text(&value, "message").is_empty() {
            message = text(&value, "message").into();
        }
        let detail = value
            .get("details")
            .map(|value| {
                if let Some(raw) = value.as_str() {
                    serde_json::from_str(raw).unwrap_or(Value::Null)
                } else {
                    value.clone()
                }
            })
            .unwrap_or(Value::Null);
        if let Some(detail) = detail.pointer("/error/message").and_then(Value::as_str) {
            message.push_str(": ");
            message.push_str(detail);
        }
        queue = queue::state(&value);
    }
    if message.is_empty() {
        message = format!("Qoder returned HTTP {status}.");
    }
    if code == "10605" || queue.as_ref().is_some_and(queue::State::busy) {
        let queue = queue.unwrap_or_default();
        let after = queue.retry_after();
        let model = queue
            .model()
            .map(|model| format!(" for {model}"))
            .unwrap_or_default();
        return Failure {
            status: 503,
            message: format!("Qoder is busy{model}; try again in {after}s."),
            retry_after: Some(after),
            sign_in: Some(String::new()),
            queue: Some(queue),
        };
    }
    if [
        "110", "112", "113", "114", "115", "116", "117", "118", "119", "122",
    ]
    .contains(&code.as_str())
    {
        return Failure::new(429, format!("Qoder usage limit reached: {message}"));
    }
    if [
        "406", "416", "430", "80411", "80412", "100400", "100401", "100403",
    ]
    .contains(&code.as_str())
    {
        return Failure::new(
            400,
            format!("Qoder refused this request ({code}): {message}"),
        );
    }
    if message
        .to_ascii_lowercase()
        .contains("error in upstream response")
    {
        return Failure::new(502, message);
    }
    if [401, 403].contains(&status) {
        return Failure::new(
            401,
            format!("Qoder refused this chat (HTTP {status}): {message}"),
        );
    }
    if status == 429 || message.to_ascii_lowercase().contains("quota") {
        return Failure::new(429, format!("Qoder usage limit reached: {message}"));
    }
    Failure::new(status, message)
}
#[derive(Debug)]
enum Event {
    Text(String),
    Reasoning(String),
    Tool {
        index: usize,
        id: String,
        name: String,
        args: String,
    },
    Args {
        index: usize,
        text: String,
    },
    Stop {
        reason: String,
        usage: Option<Value>,
    },
    Error(Failure),
}
#[derive(Default)]
struct Native {
    id: String,
    args: String,
    index: Option<usize>,
}
struct Events {
    inner: nora_ai::HttpByteStream,
    pending: Vec<u8>,
    output: VecDeque<Event>,
    splitter: Splitter,
    native: BTreeMap<u64, Native>,
    index: usize,
    usage: Option<Value>,
    finish: String,
    ended: bool,
    terminal: bool,
    answered: bool,
}
impl Events {
    fn pieces(&mut self, pieces: Vec<Piece>) {
        for piece in pieces {
            match piece {
                Piece::Text(value) if !value.is_empty() => {
                    self.answered = true;
                    self.output.push_back(Event::Text(value));
                }
                Piece::Text(_) => {}
                Piece::Call { name, args } => {
                    if self.index >= 1024 {
                        self.fail("Qoder returned too many tool calls.");
                        break;
                    }
                    self.answered = true;
                    self.output.push_back(Event::Tool {
                        index: self.index,
                        id: format!("call_{}", id()),
                        name,
                        args,
                    });
                    self.index += 1;
                }
                Piece::Error(message) => {
                    self.fail(message);
                    break;
                }
            }
        }
    }
    fn fail(&mut self, message: impl Into<String>) {
        self.ended = true;
        self.output
            .push_back(Event::Error(Failure::new(502, message)));
    }
    fn line(&mut self, line: &[u8]) {
        let Ok(line) = std::str::from_utf8(line) else {
            self.fail("Qoder returned invalid response text.");
            return;
        };
        let Some(raw) = line.trim().strip_prefix("data:").map(str::trim) else {
            return;
        };
        if raw == "[DONE]" {
            self.terminal = true;
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(raw) else {
            return;
        };
        let inner = text(&value, "body");
        if let Some(status) = value
            .get("statusCodeValue")
            .and_then(Value::as_u64)
            .filter(|status| *status != 200)
        {
            self.output.push_back(Event::Error(failure(
                status.min(u16::MAX as u64) as u16,
                if inner.is_empty() { raw } else { inner },
            )));
            self.ended = true;
            return;
        }
        if inner.trim() == "[DONE]" {
            self.terminal = true;
            return;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(inner) else {
            return;
        };
        if let Some(usage) = chunk.get("usage").filter(|usage| !usage.is_null()) {
            let prompt = usage
                .get("prompt_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let completion = usage
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let mut output = json!({"prompt_tokens":prompt,"completion_tokens":completion,"total_tokens":prompt.saturating_add(completion)});
            for field in ["prompt_tokens_details", "completion_tokens_details"] {
                if let Some(details) = usage.get(field) {
                    output[field] = details.clone();
                }
            }
            self.usage = Some(output);
        }
        let choice = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .unwrap_or(&Value::Null);
        if !text(choice, "finish_reason").is_empty() {
            self.finish = text(choice, "finish_reason").into();
        }
        let delta = choice.get("delta").unwrap_or(&Value::Null);
        if !text(delta, "reasoning_content").is_empty() {
            self.answered = true;
            self.output
                .push_back(Event::Reasoning(text(delta, "reasoning_content").into()));
        }
        if let Some(content) = delta.get("content").and_then(Value::as_str) {
            let pieces = self.splitter.feed(content);
            self.pieces(pieces);
            if self.ended {
                return;
            }
        }
        for call in delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
            if self.native.len() >= 1024 && !self.native.contains_key(&index) {
                self.fail("Qoder returned too many native tool calls.");
                return;
            }
            let function = call.get("function").unwrap_or(&Value::Null);
            let name = text(function, "name").trim();
            let args = text(function, "arguments");
            if text(call, "id").is_empty() && name.is_empty() && args.is_empty() {
                continue;
            }
            let native = self.native.entry(index).or_default();
            if !text(call, "id").is_empty() {
                native.id = text(call, "id").into();
            }
            if let Some(index) = native.index {
                if !args.is_empty() {
                    self.output.push_back(Event::Args {
                        index,
                        text: args.into(),
                    });
                }
                continue;
            }
            if native.args.len() + args.len() > 4 * 1024 * 1024 {
                self.fail("Qoder's tool arguments exceed 4 MiB.");
                return;
            }
            native.args.push_str(args);
            if name.is_empty() {
                continue;
            }
            let pieces = self.splitter.flush();
            self.pieces(pieces);
            if self.ended {
                return;
            }
            if self.index >= 1024 {
                self.fail("Qoder returned too many tool calls.");
                return;
            }
            let native = self.native.get_mut(&index).unwrap();
            native.index = Some(self.index);
            self.splitter.saw_tool = true;
            self.answered = true;
            self.output.push_back(Event::Tool {
                index: self.index,
                id: if native.id.is_empty() {
                    format!("call_{}", id())
                } else {
                    native.id.clone()
                },
                name: name.into(),
                args: std::mem::take(&mut native.args),
            });
            self.index += 1;
        }
    }
    fn finish(&mut self) {
        let pieces = self.splitter.flush();
        self.pieces(pieces);
        if self.ended {
            return;
        }
        if self.native.values().any(|call| call.index.is_none()) {
            self.fail("Qoder's native tool call has no name; it was not sent as a tool.");
            return;
        }
        if !self.terminal && self.finish.is_empty() {
            self.fail("Qoder stopped before completing its answer.");
            return;
        }
        if !self.answered && self.finish.is_empty() {
            self.fail("Qoder ended without an answer.");
            return;
        }
        self.output.push_back(Event::Stop {
            reason: if self.splitter.saw_tool {
                "tool_calls"
            } else if self.finish == "length" {
                "length"
            } else {
                "stop"
            }
            .into(),
            usage: self.usage.take(),
        });
        self.ended = true;
    }
}
fn events(
    body: nora_ai::HttpByteStream,
    tools: Vec<Value>,
) -> impl futures::Stream<Item = Event> + Send {
    let state = Events {
        inner: body,
        pending: Vec::new(),
        output: VecDeque::new(),
        splitter: Splitter::new(tools),
        native: BTreeMap::new(),
        index: 0,
        usage: None,
        finish: String::new(),
        ended: false,
        terminal: false,
        answered: false,
    };
    futures::stream::unfold(state, |mut state| async move {
        loop {
            if let Some(event) = state.output.pop_front() {
                return Some((event, state));
            }
            if state.ended {
                return None;
            }
            if let Some(end) = state.pending.iter().position(|byte| *byte == b'\n') {
                let line = state.pending.drain(..=end).collect::<Vec<_>>();
                state.line(&line);
                continue;
            }
            if state.pending.len() > 1024 * 1024 {
                state.fail("A Qoder stream event exceeds 1 MiB.");
                continue;
            }
            if state.terminal {
                state.finish();
                continue;
            }
            match state.inner.next().await {
                Some(Ok(bytes)) => {
                    if state.pending.len() + bytes.len() > 8 * 1024 * 1024 {
                        state.fail("The Qoder stream buffer exceeds 8 MiB.");
                    } else {
                        state.pending.extend_from_slice(&bytes);
                    }
                }
                Some(Err(_)) => state.fail("The Qoder response stream stopped."),
                None => {
                    if !state.pending.is_empty() {
                        let line = std::mem::take(&mut state.pending);
                        state.line(&line);
                        if !state.output.is_empty() || state.ended {
                            continue;
                        }
                    }
                    state.finish();
                }
            }
        }
    })
}
fn chunk(event: Event, id: &str, model: &str, created: u64) -> Vec<u8> {
    let (mut delta, mut finish, mut usage) = (json!({}), Value::Null, None);
    match event {
        Event::Text(text) => delta = json!({"content":text}),
        Event::Reasoning(text) => delta = json!({"reasoning_content":text}),
        Event::Tool {
            index,
            id,
            name,
            args,
        } => {
            delta = json!({"tool_calls":[{"index":index,"id":id,"type":"function","function":{"name":name,"arguments":args}}]})
        }
        Event::Args { index, text } => {
            delta = json!({"tool_calls":[{"index":index,"function":{"arguments":text}}]})
        }
        Event::Stop {
            reason,
            usage: value,
        } => {
            finish = json!(reason);
            usage = value;
        }
        Event::Error(error) => {
            return format!(
                "data: {}\n\n",
                json!({"error":{"message":error.message,"type":"qoder_error","code":error.status}})
            )
            .into_bytes()
        }
    };
    let mut value = json!({"id":id,"object":"chat.completion.chunk","created":created,"model":model,"choices":[{"index":0,"delta":delta,"finish_reason":finish}]});
    if let Some(usage) = usage {
        value["usage"] = usage;
    }
    format!("data: {value}\n\n").into_bytes()
}
pub(crate) struct Opened {
    events: std::pin::Pin<Box<dyn futures::Stream<Item = Event> + Send>>,
    first: Event,
}
pub(crate) async fn open(response: HttpStreamingResponse, chat: &Value) -> Result<Opened, Failure> {
    let tools = chat
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut events: std::pin::Pin<Box<dyn futures::Stream<Item = Event> + Send>> =
        Box::pin(events(response.body, tools));
    let Some(first) = events.next().await else {
        return Err(Failure::new(502, "Qoder ended without an answer."));
    };
    if let Event::Error(error) = first {
        return Err(error);
    }
    Ok(Opened { events, first })
}
#[cfg(test)]
pub(crate) async fn answer(
    response: HttpStreamingResponse,
    chat: &Value,
) -> PluginResult<FetchResponse> {
    match open(response, chat).await {
        Ok(opened) => answer_opened(opened, chat).await,
        Err(error) => Ok(error.response()),
    }
}
pub(crate) async fn answer_opened(opened: Opened, chat: &Value) -> PluginResult<FetchResponse> {
    let Opened { mut events, first } = opened;
    let id = format!("chatcmpl-{}", id());
    let model = text(chat, "model").to_owned();
    let created = now() / 1000;
    if chat.get("stream").and_then(Value::as_bool) != Some(true) {
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut calls = Vec::new();
        let mut finish = "stop".to_owned();
        let mut usage = None;
        let mut current = Some(first);
        let mut size = 0;
        loop {
            let event = if let Some(event) = current.take() {
                event
            } else {
                let Some(event) = events.next().await else {
                    break;
                };
                event
            };
            match event {
                Event::Text(text) => {
                    size += text.len();
                    content.push_str(&text);
                }
                Event::Reasoning(text) => {
                    size += text.len();
                    reasoning.push_str(&text);
                }
                Event::Tool { id, name, args, .. } => {
                    size += args.len();
                    calls.push(json!({"id":id,"type":"function","function":{"name":name,"arguments":args}}));
                }
                Event::Args { index, text: value } => {
                    size += value.len();
                    let call = calls.get_mut(index).ok_or_else(|| {
                        transport("Qoder returned arguments without a tool call.")
                    })?;
                    let args = format!("{}{}", text(&call["function"], "arguments"), value);
                    call["function"]["arguments"] = json!(args);
                }
                Event::Stop {
                    reason,
                    usage: value,
                } => {
                    finish = reason;
                    usage = value;
                }
                Event::Error(error) => return Ok(error.response()),
            }
            if size > 8 * 1024 * 1024 {
                return Err(transport("The Qoder completion exceeds 8 MiB."));
            }
        }
        let mut message = json!({"role":"assistant","content":content});
        if !reasoning.is_empty() {
            message["reasoning_content"] = json!(reasoning);
        }
        if !calls.is_empty() {
            message["tool_calls"] = json!(calls);
        }
        let mut value = json!({"id":id,"object":"chat.completion","created":created,"model":model,"choices":[{"index":0,"message":message,"finish_reason":finish}]});
        if let Some(usage) = usage {
            value["usage"] = usage;
        }
        return Ok(FetchResponse::bytes(
            200,
            BTreeMap::from([
                ("content-type".into(), "application/json".into()),
                ("x-magpie-sign-in".into(), "kept".into()),
            ]),
            value.to_string().into_bytes(),
        ));
    }
    let role=format!("data: {}\n\n",json!({"id":id,"object":"chat.completion.chunk","created":created,"model":model,"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]})).into_bytes();
    let state = (events, Some(first), Some(role), false, id, model, created);
    let body = futures::stream::unfold(
        state,
        |(mut events, mut first, mut role, ended, id, model, created)| async move {
            if ended {
                return None;
            }
            let mut finished = false;
            let bytes = if let Some(role) = role.take() {
                role
            } else if let Some(event) = first.take() {
                finished = matches!(event, Event::Error(_));
                chunk(event, &id, &model, created)
            } else if let Some(event) = events.next().await {
                finished = matches!(event, Event::Error(_));
                chunk(event, &id, &model, created)
            } else {
                finished = true;
                b"data: [DONE]\n\n".to_vec()
            };
            Some((
                Ok(bytes),
                (events, first, role, finished, id, model, created),
            ))
        },
    );
    Ok(FetchResponse {
        status: 200,
        headers: BTreeMap::from([
            ("content-type".into(), "text/event-stream".into()),
            ("cache-control".into(), "no-cache".into()),
            ("x-magpie-sign-in".into(), "kept".into()),
        ]),
        body: Box::pin(body),
    })
}
#[cfg(test)]
#[path = "../test/response.test.rs"]
mod tests;
#[cfg(test)]
#[path = "../test/xml.test.rs"]
mod xml_tests;
