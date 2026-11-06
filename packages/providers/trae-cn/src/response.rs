use crate::tools::{self, NativeCalls, TextParts, TextTools, ToolCall};
use crate::*;
use futures::StreamExt;
use std::collections::VecDeque;
#[derive(Clone, Debug)]
pub(crate) struct Failure {
    pub status: u16,
    pub code: String,
    pub message: String,
    pub expired: bool,
}
impl Failure {
    pub fn new(site: Site, status: u16, code: &str, message: &str) -> Self {
        let lower = message.to_ascii_lowercase();
        let auth_words = [
            "not login",
            "not logged in",
            "unauthorized",
            "unauthorised",
            "token expired",
            "token is expired",
            "token invalid",
            "token is invalid",
            "jwt",
            "未登录",
            "登录过期",
            "登录已过期",
            "登录失效",
        ]
        .iter()
        .any(|word| lower.contains(word));
        let quota_words = [
            "quota",
            "credit",
            "insufficient",
            "exceed",
            "limit",
            "额度",
            "积分",
            "次数",
            "上限",
            "用完",
        ]
        .iter()
        .any(|word| lower.contains(word));
        let expired = status == 401 || site.lapsed(code) || status == 403 && auth_words;
        let status = if expired {
            401
        } else if status == 429 || site.quota(code) || status == 403 && quota_words {
            429
        } else if status >= 400 {
            status
        } else {
            502
        };
        Self {
            status,
            code: code.into(),
            message: format!(
                "{}: {}",
                site.name(),
                if message.is_empty() {
                    format!("HTTP {status}")
                } else {
                    message.to_owned()
                }
            ),
            expired,
        }
    }
    pub fn response(self) -> FetchResponse {
        let mut headers = BTreeMap::from([
            ("content-type".into(), "application/json".into()),
            (
                "x-magpie-sign-in".into(),
                if self.expired { "expired" } else { "kept" }.into(),
            ),
        ]);
        headers.remove("content-length");
        FetchResponse::bytes(self.status,headers,json!({"error":{"message":self.message,"type":if self.status==429{"rate_limit_error"}else{"api_error"},"code":if self.code.is_empty(){Value::Null}else{json!(self.code)}}}).to_string().into_bytes())
    }
}
struct Raw {
    event: String,
    data: Value,
}
struct Sse {
    inner: nora_ai::HttpByteStream,
    pending: Vec<u8>,
    event: String,
    data: Vec<String>,
    output: VecDeque<PluginResult<Raw>>,
    ended: bool,
}
impl Sse {
    fn flush(&mut self) {
        if self.data.is_empty() {
            return;
        }
        let text = self.data.join("\n");
        self.data.clear();
        let value = serde_json::from_str::<Value>(&text).unwrap_or(json!(text));
        let event = std::mem::take(&mut self.event);
        let event = if event.is_empty() {
            value
                .get("event")
                .or_else(|| value.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        } else {
            event
        };
        self.output.push_back(Ok(Raw { event, data: value }));
    }
    fn line(&mut self, bytes: &[u8]) {
        let line = match std::str::from_utf8(bytes) {
            Ok(line) => line.trim_end_matches(['\r', '\n']),
            Err(_) => {
                self.ended = true;
                self.output
                    .push_back(Err(transport("Trae returned invalid response text.")));
                return;
            }
        };
        if line.is_empty() {
            self.flush();
            return;
        }
        if line.starts_with(':') {
            return;
        }
        if let Some(event) = line.strip_prefix("event:") {
            self.flush();
            self.event = event.trim().into();
        } else if let Some(data) = line.strip_prefix("data:") {
            self.data.push(data.trim_start().into());
            if self.data.iter().map(String::len).sum::<usize>() > 1024 * 1024 {
                self.ended = true;
                self.output
                    .push_back(Err(transport("A Trae stream event exceeds 1 MiB.")));
            }
        }
    }
}
fn sse(body: nora_ai::HttpByteStream) -> impl futures::Stream<Item = PluginResult<Raw>> + Send {
    let state = Sse {
        inner: body,
        pending: Vec::new(),
        event: String::new(),
        data: Vec::new(),
        output: VecDeque::new(),
        ended: false,
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
                state.ended = true;
                return Some((Err(transport("A Trae event line exceeds 1 MiB.")), state));
            }
            match state.inner.next().await {
                Some(Ok(bytes)) => {
                    if state.pending.len() + bytes.len() > 8 * 1024 * 1024 {
                        state.ended = true;
                        return Some((
                            Err(transport("The Trae response buffer exceeds 8 MiB.")),
                            state,
                        ));
                    }
                    state.pending.extend_from_slice(&bytes);
                }
                Some(Err(_)) => {
                    state.ended = true;
                    return Some((Err(transport("The Trae response stream stopped.")), state));
                }
                None => {
                    if !state.pending.is_empty() {
                        let bytes = std::mem::take(&mut state.pending);
                        state.line(&bytes);
                    }
                    state.flush();
                    state.ended = true;
                }
            }
        }
    })
}
fn event_name(name: &str) -> String {
    let mut out = String::new();
    let mut previous = false;
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            if character.is_ascii_uppercase() && previous {
                out.push('_');
            }
            out.push(character.to_ascii_lowercase());
            previous = character.is_ascii_lowercase() || character.is_ascii_digit();
        } else {
            if !out.ends_with('_') {
                out.push('_');
            }
            previous = false;
        }
    }
    out.trim_matches('_').into()
}
pub(crate) fn tokens(value: &Value) -> Option<Value> {
    if !value.is_object() {
        return None;
    }
    let first = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| value.get(*key).and_then(number))
            .unwrap_or(0.0)
            .max(0.0) as u64
    };
    let prompt = first(&[
        "prompt_tokens",
        "input_tokens",
        "promptTokens",
        "inputTokens",
    ]);
    let completion = first(&[
        "completion_tokens",
        "output_tokens",
        "completionTokens",
        "outputTokens",
    ]);
    if prompt == 0 && completion == 0 {
        return None;
    }
    let total = first(&["total_tokens", "totalTokens"]);
    let mut out = json!({"prompt_tokens":prompt,"completion_tokens":completion,"total_tokens":if total>0{total}else{prompt.saturating_add(completion)}});
    let read = value
        .pointer("/prompt_tokens_details/cached_tokens")
        .and_then(number)
        .unwrap_or(0.0)
        .max(0.0) as u64;
    let write = value
        .pointer("/prompt_tokens_details/cache_write_tokens")
        .and_then(number)
        .unwrap_or(0.0)
        .max(0.0) as u64;
    if read > 0 || write > 0 {
        out["prompt_tokens_details"] = json!({"cached_tokens":read});
        if write > 0 {
            out["prompt_tokens_details"]["cache_write_tokens"] = json!(write);
        }
    } else {
        for (keys, name) in [
            (
                ["cache_read_input_tokens", "cacheReadInputTokens"],
                "cache_read_input_tokens",
            ),
            (
                ["cache_creation_input_tokens", "cacheCreationInputTokens"],
                "cache_creation_input_tokens",
            ),
        ] {
            let count = first(&keys);
            if count > 0 {
                out[name] = json!(count);
            }
        }
    }
    let reasoning = value
        .get("reasoning_tokens")
        .or_else(|| value.pointer("/completion_tokens_details/reasoning_tokens"))
        .and_then(number)
        .unwrap_or(0.0)
        .max(0.0) as u64;
    if reasoning > 0 {
        out["completion_tokens_details"] = json!({"reasoning_tokens":reasoning});
    }
    Some(out)
}
#[derive(Default)]
struct Thoughts {
    all: String,
    last: String,
}
impl Thoughts {
    fn add(&mut self, said: &str) -> PluginResult<String> {
        let last = std::mem::replace(&mut self.last, said.to_owned());
        if said == last && said.len() >= 8
            || self.all.len() >= 16 && said.len() >= 16 && self.all.ends_with(said)
        {
            return Ok(String::new());
        }
        let said = if self.all.len() >= 16 && said.starts_with(&self.all) {
            &said[self.all.len()..]
        } else {
            said
        };
        if self.all.len() + said.len() > 8 * 1024 * 1024 {
            return Err(transport("Trae's reasoning exceeds 8 MiB."));
        }
        self.all.push_str(said);
        Ok(said.into())
    }
}
enum Part {
    Text(String),
    Reasoning(String),
    Call(ToolCall),
    Usage(Value),
    Finish(String),
    Error(Failure),
}
struct Parts {
    raw: std::pin::Pin<Box<dyn futures::Stream<Item = PluginResult<Raw>> + Send>>,
    text: TextTools,
    native: NativeCalls,
    thoughts: Thoughts,
    tools: Vec<Value>,
    output: VecDeque<Part>,
    finish: String,
    site: Site,
    ended: bool,
    saw_answer: bool,
}
impl Parts {
    fn fail(&mut self, error: PluginError) {
        self.output.push_back(Part::Error(Failure::new(
            self.site,
            502,
            "",
            &error.message,
        )));
        self.ended = true;
    }
    fn text_parts(&mut self, parts: TextParts) -> PluginResult<()> {
        if !parts.reasoning.is_empty() {
            let thought = self.thoughts.add(&parts.reasoning)?;
            if !thought.is_empty() {
                self.saw_answer = true;
                self.output.push_back(Part::Reasoning(thought));
            }
        }
        if !parts.text.is_empty() {
            self.saw_answer = true;
            self.output.push_back(Part::Text(parts.text));
        }
        for mut call in parts.calls {
            if call.id.is_empty() {
                call.id = format!("call_{}", id());
            }
            let call = tools::normalize_call(call, &self.tools)?;
            self.saw_answer = true;
            self.output.push_back(Part::Call(call));
        }
        match parts.error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
    fn raw(&mut self, raw: Raw) -> PluginResult<()> {
        let name = event_name(&raw.event);
        if raw.data == "[DONE]" {
            return self.finish();
        }
        let value = raw.data;
        let (code, message) = error_of(&value);
        if name == "error"
            || name != "output"
                && !code.is_empty()
                && code != "0"
                && !message.is_empty()
                && value
                    .get("response")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                && value
                    .get("content")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
        {
            self.output.push_back(Part::Error(Failure::new(
                self.site,
                502,
                &code,
                if message.is_empty() {
                    "Trae returned an error."
                } else {
                    &message
                },
            )));
            self.ended = true;
            return Ok(());
        }
        if name == "token_usage" {
            if let Some(usage) = tokens(value.get("usage").unwrap_or(&value)) {
                self.output.push_back(Part::Usage(usage));
            }
            return Ok(());
        }
        if ["done", "response_done", "stream_done"].contains(&name.as_str()) {
            if let Some(usage) = value.get("usage").and_then(tokens) {
                self.output.push_back(Part::Usage(usage));
            }
            self.set_finish(&value);
            return self.finish();
        }
        if ![
            "",
            "output",
            "message",
            "delta",
            "content",
            "text",
            "answer",
            "chunk",
            "output_delta",
            "message_delta",
        ]
        .contains(&name.as_str())
        {
            return Ok(());
        }
        let delta = value
            .get("delta")
            .filter(|delta| delta.is_object())
            .unwrap_or(&Value::Null);
        let thought = [
            text(&value, "reasoning_content"),
            text(&value, "reasoning"),
            text(delta, "reasoning_content"),
            text(delta, "reasoning"),
        ]
        .into_iter()
        .find(|thought| !thought.is_empty())
        .unwrap_or("");
        if !thought.is_empty() {
            let said = self.thoughts.add(thought)?;
            if !said.is_empty() {
                self.saw_answer = true;
                self.output.push_back(Part::Reasoning(said));
            }
        }
        let text = [
            value.get("response"),
            value.get("content"),
            delta.get("content"),
            value.get("delta"),
        ]
        .into_iter()
        .flatten()
        .find_map(Value::as_str)
        .unwrap_or("");
        if !text.is_empty()
            && !text.starts_with("Building prompt:")
            && !text.starts_with("Completed building prompt")
        {
            let parts = self.text.push(text, false)?;
            self.text_parts(parts)?;
        }
        for call in value
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            self.native.push(call)?;
        }
        if let Some(usage) = value.get("usage").and_then(tokens) {
            self.output.push_back(Part::Usage(usage));
        }
        self.set_finish(&value);
        Ok(())
    }
    fn set_finish(&mut self, value: &Value) {
        let reason = value
            .get("finish_reason")
            .or_else(|| value.get("stop_reason"))
            .or_else(|| value.get("finishReason"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        if ["length", "max_tokens", "max_output_tokens", "max_length"].contains(&reason.as_str()) {
            self.finish = "length".into();
        } else if ["content_filter", "sensitive"].contains(&reason.as_str()) {
            self.finish = "content_filter".into();
        }
    }
    fn finish(&mut self) -> PluginResult<()> {
        let parts = self.text.push("", true)?;
        self.text_parts(parts)?;
        for call in self.native.take(&self.tools)? {
            let call = tools::normalize_call(call, &self.tools)?;
            self.saw_answer = true;
            self.output.push_back(Part::Call(call));
        }
        if !self.saw_answer && self.finish.is_empty() {
            return Err(transport("Trae ended without an answer."));
        }
        self.output.push_back(Part::Finish(self.finish.clone()));
        self.ended = true;
        Ok(())
    }
}
fn parts(
    body: nora_ai::HttpByteStream,
    site: Site,
    tools: Vec<Value>,
) -> impl futures::Stream<Item = Part> + Send {
    let state = Parts {
        raw: Box::pin(sse(body)),
        text: TextTools::new(tools.clone()),
        native: NativeCalls::default(),
        thoughts: Thoughts::default(),
        tools,
        output: VecDeque::new(),
        finish: String::new(),
        site,
        ended: false,
        saw_answer: false,
    };
    futures::stream::unfold(state, |mut state| async move {
        loop {
            if let Some(part) = state.output.pop_front() {
                return Some((part, state));
            }
            if state.ended {
                return None;
            }
            let result = match state.raw.next().await {
                Some(Ok(raw)) => state.raw(raw),
                Some(Err(error)) => Err(error),
                None => state.finish(),
            };
            if let Err(error) = result {
                state.fail(error);
            }
        }
    })
}
fn event(
    delta: Value,
    finish: Option<&str>,
    usage: Option<Value>,
    id: &str,
    model: &str,
    created: u64,
) -> Vec<u8> {
    let mut value = json!({"id":id,"object":"chat.completion.chunk","created":created,"model":model,"choices":[{"index":0,"delta":delta,"finish_reason":finish}]});
    if let Some(usage) = usage {
        value["usage"] = usage;
    }
    format!("data: {value}\n\n").into_bytes()
}
pub(crate) async fn prepare(
    response: HttpStreamingResponse,
    site: Site,
    chat: &Value,
) -> PluginResult<Result<FetchResponse, Failure>> {
    let tools = chat
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut inner = Box::pin(parts(response.body, site, tools));
    let mut held = VecDeque::new();
    loop {
        let Some(part) = inner.next().await else {
            return Ok(Err(Failure::new(
                site,
                502,
                "",
                "Trae ended without an answer.",
            )));
        };
        if let Part::Error(error) = part {
            return Ok(Err(error));
        }
        let usage = matches!(part, Part::Usage(_));
        held.push_back(part);
        if !usage {
            break;
        }
    }
    let model = text(chat, "model").to_owned();
    let id = format!("chatcmpl-{}", id());
    let created = now() / 1000;
    if chat.get("stream").and_then(Value::as_bool) != Some(true) {
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut calls = Vec::new();
        let mut usage = None;
        let mut finish = String::new();
        let mut size = 0;
        loop {
            let part = if let Some(part) = held.pop_front() {
                part
            } else {
                let Some(part) = inner.next().await else {
                    break;
                };
                part
            };
            match part {
                Part::Text(value) => {
                    size += value.len();
                    text.push_str(&value);
                }
                Part::Reasoning(value) => {
                    size += value.len();
                    reasoning.push_str(&value);
                }
                Part::Call(call) => {
                    size += call.arguments.len();
                    calls.push(json!({"id":call.id,"type":"function","function":{"name":call.name,"arguments":call.arguments}}));
                }
                Part::Usage(value) => usage = Some(value),
                Part::Finish(value) => finish = value,
                Part::Error(error) => return Ok(Ok(error.response())),
            }
            if size > 8 * 1024 * 1024 {
                return Err(transport("The Trae completion exceeds 8 MiB."));
            }
        }
        let mut message = json!({"role":"assistant","content":if text.is_empty()&&!calls.is_empty(){Value::Null}else{json!(text)}});
        if !reasoning.is_empty() {
            message["reasoning_content"] = json!(reasoning);
        }
        let finish = if !calls.is_empty() {
            message["tool_calls"] = json!(calls);
            "tool_calls"
        } else if finish.is_empty() {
            "stop"
        } else {
            &finish
        };
        let mut body = json!({"id":id,"object":"chat.completion","created":created,"model":model,"choices":[{"index":0,"message":message,"finish_reason":finish}]});
        if let Some(usage) = usage {
            body["usage"] = usage;
        }
        return Ok(Ok(FetchResponse::bytes(
            200,
            BTreeMap::from([
                ("content-type".into(), "application/json".into()),
                ("x-magpie-sign-in".into(), "kept".into()),
            ]),
            body.to_string().into_bytes(),
        )));
    }
    let mut queue = VecDeque::from([event(
        json!({"role":"assistant","content":""}),
        None,
        None,
        &id,
        &model,
        created,
    )]);
    let state = (
        inner,
        held,
        std::mem::take(&mut queue),
        0usize,
        None,
        String::new(),
        false,
        id,
        model,
        created,
    );
    let body = futures::stream::unfold(
        state,
        |(
            mut inner,
            mut held,
            mut queue,
            mut calls,
            mut usage,
            mut finish,
            ended,
            id,
            model,
            created,
        )| async move {
            loop {
                if let Some(bytes) = queue.pop_front() {
                    return Some((
                        Ok(bytes),
                        (
                            inner, held, queue, calls, usage, finish, ended, id, model, created,
                        ),
                    ));
                }
                if ended {
                    return None;
                }
                let part = if let Some(part) = held.pop_front() {
                    Some(part)
                } else {
                    inner.next().await
                };
                match part {
                    Some(Part::Text(text)) => queue.push_back(event(
                        json!({"content":text}),
                        None,
                        None,
                        &id,
                        &model,
                        created,
                    )),
                    Some(Part::Reasoning(text)) => queue.push_back(event(
                        json!({"reasoning_content":text}),
                        None,
                        None,
                        &id,
                        &model,
                        created,
                    )),
                    Some(Part::Call(call)) => {
                        queue.push_back(event(json!({"tool_calls":[{"index":calls,"id":call.id,"type":"function","function":{"name":call.name,"arguments":call.arguments}}]}),None,None,&id,&model,created));
                        calls += 1;
                    }
                    Some(Part::Usage(value)) => usage = Some(value),
                    Some(Part::Finish(value)) => finish = value,
                    Some(Part::Error(error)) => {
                        queue.push_back(format!("data: {}\n\n",json!({"error":{"message":error.message,"type":"api_error","code":error.code}})).into_bytes());
                        queue.push_back(b"data: [DONE]\n\n".to_vec());
                        return queue.pop_front().map(|bytes| {
                            (
                                Ok(bytes),
                                (
                                    inner, held, queue, calls, usage, finish, true, id, model,
                                    created,
                                ),
                            )
                        });
                    }
                    None => {
                        let reason = if calls > 0 {
                            "tool_calls"
                        } else if finish.is_empty() {
                            "stop"
                        } else {
                            &finish
                        };
                        queue.push_back(event(json!({}), Some(reason), None, &id, &model, created));
                        if let Some(usage) = usage.take() {
                            queue.push_back(format!("data: {}\n\n",json!({"id":id,"object":"chat.completion.chunk","created":created,"model":model,"choices":[],"usage":usage})).into_bytes());
                        }
                        queue.push_back(b"data: [DONE]\n\n".to_vec());
                        return queue.pop_front().map(|bytes| {
                            (
                                Ok(bytes),
                                (
                                    inner, held, queue, calls, usage, finish, true, id, model,
                                    created,
                                ),
                            )
                        });
                    }
                }
            }
        },
    );
    Ok(Ok(FetchResponse {
        status: 200,
        headers: BTreeMap::from([
            ("content-type".into(), "text/event-stream".into()),
            ("cache-control".into(), "no-cache".into()),
            ("x-magpie-sign-in".into(), "kept".into()),
        ]),
        body: Box::pin(body),
    }))
}
#[cfg(test)]
#[path = "../test/response.test.rs"]
mod tests;
#[cfg(test)]
#[path = "../test/xml.test.rs"]
mod xml_tests;
