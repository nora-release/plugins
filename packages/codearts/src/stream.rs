use crate::provider::*;
use futures::StreamExt;
use nora_ai::HttpStreamingResponse;
use nora_provider_plugin_sdk::FetchResponse;
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use tokio::sync::OwnedSemaphorePermit;

pub(super) struct Head {
    pub response: HttpStreamingResponse,
    pub bytes: Vec<u8>,
    pub code: Option<String>,
    pub message: String,
}

pub(super) async fn peek(mut response: HttpStreamingResponse) -> PluginResult<Head> {
    let mut bytes = Vec::new();
    loop {
        let text = String::from_utf8_lossy(&bytes);
        if events(&text).iter().any(content)
            || serde_json::from_slice::<Value>(&bytes)
                .is_ok_and(|value| value.get("choices").is_some_and(Value::is_array))
        {
            return Ok(Head {
                response,
                bytes,
                code: None,
                message: String::new(),
            });
        }
        let (code, message) = super::error_parts(&text);
        if !code.is_empty() {
            return Ok(Head {
                response,
                bytes,
                code: Some(code),
                message,
            });
        }
        if bytes.len() >= 16 * 1024 {
            return Ok(Head {
                response,
                bytes,
                code: None,
                message: String::new(),
            });
        }
        let chunk = response.body.next().await;
        match chunk {
            Some(Ok(chunk)) => {
                if bytes.len() + chunk.len() > 1024 * 1024 {
                    return Err(PluginError::transport(
                        "The CodeArts response head exceeds 1 MiB.",
                    ));
                }
                bytes.extend_from_slice(&chunk);
            }
            Some(Err(_)) => {
                return Err(PluginError::transport(
                    "The CodeArts response stopped before its first answer.",
                ))
            }
            None => {
                return Ok(Head {
                    response,
                    bytes,
                    code: Some("empty_response".into()),
                    message: "CodeArts ended the response before returning an answer.".into(),
                })
            }
        }
    }
}
fn events(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("data:"))
        .filter_map(|data| serde_json::from_str(data.trim()).ok())
        .collect()
}
fn content(value: &Value) -> bool {
    let Some(choice) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
    else {
        return false;
    };
    choice
        .get("finish_reason")
        .is_some_and(|reason| !reason.is_null())
        || choice.get("message").is_some()
        || choice.get("delta").is_some_and(|delta| {
            ["content", "reasoning_content"]
                .iter()
                .any(|field| !text(delta, field).is_empty())
                || delta
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .is_some_and(|calls| !calls.is_empty())
        })
}

struct State {
    inner: nora_ai::HttpByteStream,
    pending: Vec<u8>,
    output: VecDeque<Vec<u8>>,
    ended: bool,
    _permit: OwnedSemaphorePermit,
}
pub(super) fn replay(
    response: HttpStreamingResponse,
    head: Vec<u8>,
    permit: OwnedSemaphorePermit,
) -> FetchResponse {
    let mut headers = response.headers;
    headers.retain(|name, _| {
        !matches!(
            name.to_ascii_lowercase().as_str(),
            "content-length" | "content-encoding"
        )
    });
    headers
        .entry("x-magpie-sign-in".into())
        .or_insert_with(|| "kept".into());
    if !(200..300).contains(&response.status) {
        let body =
            futures::stream::unfold((response.body, permit), |(mut inner, permit)| async move {
                inner.next().await.map(|chunk| {
                    (
                        chunk.map(|bytes| bytes.to_vec()).map_err(|_| {
                            PluginError::transport("The CodeArts response stream stopped.")
                        }),
                        (inner, permit),
                    )
                })
            });
        return FetchResponse {
            status: response.status,
            headers,
            body: Box::pin(body),
        };
    }
    // Some gateway versions ignore stream=true. A complete JSON answer is
    // converted into one chunk so the caller still gets the requested SSE.
    let (head, body) = if let Ok(value) = serde_json::from_slice::<Value>(&head) {
        if let Some(choices) = value.get("choices").and_then(Value::as_array) {
            let mut chunk = value.clone();
            chunk["object"] = json!("chat.completion.chunk");
            chunk["choices"] = json!(choices
                .iter()
                .map(|choice| {
                    let mut choice = choice.clone();
                    if let Some(message) = choice
                        .as_object_mut()
                        .and_then(|choice| choice.remove("message"))
                    {
                        choice["delta"] = message;
                    }
                    choice
                })
                .collect::<Vec<_>>());
            headers.insert("content-type".into(), "text/event-stream".into());
            (
                format!("data: {chunk}\n\ndata: [DONE]\n\n").into_bytes(),
                Box::pin(futures::stream::empty()) as nora_ai::HttpByteStream,
            )
        } else {
            (head, response.body)
        }
    } else {
        (head, response.body)
    };
    let inner = Box::pin(futures::stream::once(async move { Ok(head.into()) }).chain(body));
    let state = State {
        inner,
        pending: Vec::new(),
        output: VecDeque::new(),
        ended: false,
        _permit: permit,
    };
    let body = futures::stream::unfold(state, |mut state| async move {
        loop {
            if let Some(bytes) = state.output.pop_front() {
                return Some((Ok(bytes), state));
            }
            if state.ended {
                return None;
            }
            match state.inner.next().await {
                Some(Ok(bytes)) => state.pending.extend_from_slice(&bytes),
                Some(Err(_)) => {
                    state.ended = true;
                    return Some((
                        Err(PluginError::transport(
                            "The CodeArts response stream stopped.",
                        )),
                        state,
                    ));
                }
                None => state.ended = true,
            }
            if state.pending.len() > 1024 * 1024 {
                state.ended = true;
                return Some((
                    Err(PluginError::transport(
                        "A CodeArts stream event exceeds 1 MiB.",
                    )),
                    state,
                ));
            }
            while let Some(end) = state.pending.iter().position(|byte| *byte == b'\n') {
                let line = state.pending.drain(..=end).collect::<Vec<_>>();
                let (code, message) = super::error_parts(&String::from_utf8_lossy(&line));
                if !code.is_empty() {
                    state.output.push_back(format!("data: {}\n\n",json!({"error":{"message":message,"code":code,"type":if super::error_status(&code)==429{"rate_limit_error"}else{"provider_error"}}})).into_bytes());
                    state.pending.clear();
                    state.ended = true;
                    break;
                }
                state.output.push_back(line);
            }
            if state.ended && !state.pending.is_empty() {
                state.output.push_back(std::mem::take(&mut state.pending));
            }
        }
    });
    FetchResponse {
        status: response.status,
        headers,
        body: Box::pin(body),
    }
}

pub(super) async fn fold(
    response: HttpStreamingResponse,
    head: Vec<u8>,
    permit: OwnedSemaphorePermit,
) -> PluginResult<FetchResponse> {
    let mut response = replay(response, head, permit);
    let mut bytes = Vec::new();
    while let Some(chunk) = response.body.next().await {
        let chunk = chunk?;
        if bytes.len() + chunk.len() > 8 * 1024 * 1024 {
            return Err(PluginError::transport(
                "The CodeArts completion exceeds 8 MiB.",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
        if value.get("choices").is_some_and(Value::is_array) {
            return Ok(FetchResponse::bytes(
                response.status,
                BTreeMap::from([("content-type".into(), "application/json".into())]),
                bytes,
            ));
        }
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| PluginError::transport("CodeArts returned invalid completion text."))?;
    let value = completion(&events(&text), text.contains("[DONE]"))?;
    Ok(FetchResponse::bytes(
        200,
        BTreeMap::from([
            ("content-type".into(), "application/json".into()),
            ("x-magpie-sign-in".into(), "kept".into()),
        ]),
        value.to_string().into_bytes(),
    ))
}

pub(super) fn completion(events: &[Value], done: bool) -> PluginResult<Value> {
    let mut id = String::new();
    let mut model = String::new();
    let mut created = now() / 1000;
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut calls: BTreeMap<u64, Value> = BTreeMap::new();
    let mut finish = Value::Null;
    let mut usage = Value::Null;
    for event in events {
        if event.get("error").is_some() {
            return Err(PluginError::transport(
                "CodeArts returned an error instead of a complete answer.",
            ));
        }
        if !text(event, "id").is_empty() {
            id = text(event, "id").into();
        }
        if !text(event, "model").is_empty() {
            model = text(event, "model").into();
        }
        if event.get("usage").is_some_and(|usage| !usage.is_null()) {
            usage = event["usage"].clone();
        }
        if let Some(at) = event.get("created").and_then(Value::as_u64) {
            created = at;
        }
        for choice in event
            .get("choices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if choice
                .get("finish_reason")
                .is_some_and(|reason| !reason.is_null())
            {
                finish = choice["finish_reason"].clone();
            }
            let delta = choice
                .get("delta")
                .or_else(|| choice.get("message"))
                .unwrap_or(&Value::Null);
            content.push_str(text(delta, "content"));
            reasoning.push_str(text(delta, "reasoning_content"));
            for call in delta
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                if calls.len() >= 1024 && !calls.contains_key(&index) {
                    return Err(invalid("CodeArts returned too many tool calls."));
                }
                let current = calls.entry(index).or_insert_with(
                    || json!({"id":"","type":"function","function":{"name":"","arguments":""}}),
                );
                if !text(call, "id").is_empty() {
                    current["id"] = call["id"].clone();
                }
                if let Some(function) = call.get("function") {
                    for field in ["name", "arguments"] {
                        let value = format!(
                            "{}{}",
                            text(&current["function"], field),
                            text(function, field)
                        );
                        current["function"][field] = json!(value);
                    }
                }
            }
        }
    }
    if !done && finish.is_null() {
        return Err(PluginError::transport(
            "CodeArts stopped before completing its answer.",
        ));
    }
    if events.iter().all(|event| {
        !event
            .get("choices")
            .and_then(Value::as_array)
            .is_some_and(|choices| !choices.is_empty())
    }) {
        return Err(PluginError::transport("CodeArts returned no answer."));
    }
    if id.is_empty() {
        id = format!("chatcmpl-{}", crate::sign::id(12)?);
    }
    if finish.is_null() {
        finish = json!(if calls.is_empty() {
            "stop"
        } else {
            "tool_calls"
        });
    }
    let mut message = json!({"role":"assistant","content":content});
    if !reasoning.is_empty() {
        message["reasoning_content"] = json!(reasoning);
    }
    if !calls.is_empty() {
        message["tool_calls"] = json!(calls.into_values().collect::<Vec<_>>());
    }
    Ok(
        json!({"id":id,"object":"chat.completion","created":created,"model":model,"choices":[{"index":0,"message":message,"finish_reason":finish}],"usage":usage}),
    )
}

#[cfg(test)]
#[path = "../test/stream.test.rs"]
mod tests;
