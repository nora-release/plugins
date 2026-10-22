use super::*;
use futures::{stream, StreamExt};
use nora_ai::{HttpStreamingResponse, SseParser};
use nora_provider_plugin_sdk::FetchResponse;
use std::collections::{BTreeMap, VecDeque};

struct State {
    body: nora_ai::HttpByteStream,
    parser: SseParser,
    line: Vec<u8>,
    queued: VecDeque<PluginResult<Vec<u8>>>,
    names: envelope::Names,
    text_calls: super::text_call::TextCalls,
    done: bool,
    prompt_restricted: bool,
}
impl State {
    fn events(&mut self, events: Vec<nora_ai::SseEvent>) {
        for event in events {
            if event.data.trim().is_empty() || event.data.trim() == "[DONE]" {
                continue;
            }
            let decoded = serde_json::from_str::<Value>(&event.data).map_err(|_| {
                PluginError::transport("Code Assist returned an invalid stream event.")
            });
            let normalized = decoded.and_then(|outer| {
                if let Some(error) = outer
                    .get("error")
                    .or_else(|| outer.pointer("/response/error"))
                {
                    let code = error
                        .get("code")
                        .and_then(Value::as_u64)
                        .unwrap_or(502)
                        .clamp(400, 599) as u16;
                    return encode(&safe_error(
                        code,
                        self.prompt_restricted
                            && error.get("message").and_then(Value::as_str).is_some_and(
                                |message| {
                                    message
                                        .to_ascii_lowercase()
                                        .contains("resource has been exhausted")
                                },
                            ),
                    ));
                }
                let mut response = outer.get("response").cloned().unwrap_or(outer);
                for candidate in response["candidates"].as_array_mut().into_iter().flatten() {
                    let mut parts = Vec::new();
                    for mut part in candidate["content"]["parts"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                    {
                        if part["thought"] != true && part.get("thoughtSignature").is_none() {
                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                parts.extend(self.text_calls.feed(text, &self.names));
                                continue;
                            }
                        }
                        parts.extend(self.text_calls.flush());
                        if let Some(call) = part.get_mut("functionCall") {
                            if let Some(name) = call["name"].as_str() {
                                call["name"] = json!(self.names.restore(name));
                            }
                        }
                        parts.push(part);
                    }
                    if candidate
                        .get("finishReason")
                        .and_then(Value::as_str)
                        .is_some_and(|reason| !reason.is_empty())
                    {
                        parts.extend(self.text_calls.flush());
                    }
                    if !parts.is_empty() || candidate.get("content").is_some() {
                        candidate["content"]["parts"] = json!(parts);
                    }
                }
                if response.get("candidates").is_none()
                    && response.get("usageMetadata").is_none()
                    && response.get("promptFeedback").is_none()
                {
                    return Err(PluginError::transport(
                        "Code Assist returned an empty stream event.",
                    ));
                }
                encode(&response)
            });
            self.queued.push_back(normalized);
        }
    }
}

fn encode(response: &Value) -> PluginResult<Vec<u8>> {
    let mut encoded = b"data: ".to_vec();
    encoded.extend(
        serde_json::to_vec(response)
            .map_err(|_| PluginError::transport("The Code Assist stream cannot be encoded."))?,
    );
    encoded.extend_from_slice(b"\n\n");
    Ok(encoded)
}

fn safe_error(status: u16, prompt_refused: bool) -> Value {
    let (status, message) = if prompt_refused {
        (
            400,
            "Antigravity did not accept this system prompt. Use another provider for this request.",
        )
    } else {
        (
            status,
            match status {
                401 => "The Google sign-in was refused.",
                403 => "Google did not grant permission for this request.",
                429 => "Google rate limit or quota exhausted.",
                400 => "Google rejected this request.",
                _ => "The Google service could not complete this request.",
            },
        )
    };
    json!({"error":{"code":status,"message":message}})
}

pub(crate) async fn response(
    response: HttpStreamingResponse,
    names: envelope::Names,
    secrets: Vec<String>,
    prompt_restricted: bool,
) -> PluginResult<FetchResponse> {
    let response = if response.status == 429 && prompt_restricted {
        let text = nora_ai::collect_http_body_text_limited(response.body, 1024 * 1024 + 1)
            .await
            .map_err(|_| PluginError::transport("The Google error response was incomplete."))?;
        if text.len() > 1024 * 1024 {
            return Err(PluginError::transport(
                "The Google error response is too large.",
            ));
        }
        if text
            .to_ascii_lowercase()
            .contains("resource has been exhausted")
        {
            return Ok(FetchResponse::bytes(
                400,
                BTreeMap::from([("content-type".into(), "application/json".into())]),
                serde_json::to_vec(&safe_error(400, true)).expect("fixed error response"),
            ));
        }
        HttpStreamingResponse {
            status: response.status,
            headers: response.headers,
            body: Box::pin(stream::once(async move { Ok(text.into_bytes().into()) })),
        }
    } else {
        response
    };
    if !(200..300).contains(&response.status) {
        return nora_provider::transport::finish_response(response, secrets).await;
    }
    let state = State {
        body: response.body,
        parser: SseParser::new(),
        line: Vec::new(),
        queued: VecDeque::new(),
        names,
        text_calls: super::text_call::TextCalls::default(),
        done: false,
        prompt_restricted,
    };
    let body = stream::unfold(state, |mut state| async move {
        loop {
            if let Some(item) = state.queued.pop_front() {
                if item.is_err() {
                    state.done = true;
                    state.queued.clear();
                }
                return Some((item, state));
            }
            if state.done {
                return None;
            }
            match state.body.next().await {
                Some(Ok(bytes)) => {
                    for byte in bytes {
                        state.line.push(byte);
                        if state.line.len() > 32 * 1024 * 1024 {
                            state.done = true;
                            return Some((
                                Err(PluginError::transport(
                                    "A Code Assist stream event is too large.",
                                )),
                                state,
                            ));
                        }
                        if byte == b'\n' {
                            let Ok(line) = std::str::from_utf8(&state.line) else {
                                state.done = true;
                                return Some((
                                    Err(PluginError::transport(
                                        "The Code Assist stream is not valid UTF-8.",
                                    )),
                                    state,
                                ));
                            };
                            state.parser.ingest(line);
                            state.line.clear();
                            let events = state.parser.drain();
                            state.events(events);
                        }
                    }
                }
                Some(Err(_)) => {
                    state.done = true;
                    return Some((
                        Err(PluginError::transport(
                            "The Code Assist stream stopped before it finished.",
                        )),
                        state,
                    ));
                }
                None => {
                    if !state.line.is_empty() {
                        let Ok(line) = std::str::from_utf8(&state.line) else {
                            state.done = true;
                            return Some((
                                Err(PluginError::transport(
                                    "The Code Assist stream is not valid UTF-8.",
                                )),
                                state,
                            ));
                        };
                        state.parser.ingest(&format!("{line}\n"));
                        state.line.clear();
                    }
                    let events = state.parser.finish();
                    state.events(events);
                    let held = state.text_calls.flush();
                    if !held.is_empty() {
                        state
                            .queued
                            .push_back(encode(&json!({"candidates":[{"content":{"parts":held}}]})));
                    }
                    state.done = true;
                }
            }
        }
    });
    Ok(FetchResponse {
        status: response.status,
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
