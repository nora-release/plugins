use crate::catalog;
use futures::{stream::BoxStream, StreamExt};
use nora_provider_plugin_sdk::{FetchResponse, PluginError, PluginResult};
use nora_provider_subscription_core as core;
use serde_json::{json, Value};
use std::{collections::BTreeMap, time::Duration};
const MAX_FRAME: usize = 32 * 1024 * 1024;
fn text(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_owned();
    }
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["type"] == "text")
        .filter_map(|p| p["text"].as_str())
        .collect()
}
pub(crate) fn fit_effort(want: &str, levels: &[String]) -> String {
    let want = if want == "ultra" && !levels.iter().any(|s| s == want) {
        "max"
    } else {
        want
    };
    let ranks = [
        "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
    ];
    if levels.is_empty() || levels.iter().any(|s| s == want) {
        return want.to_owned();
    }
    let Some(at) = ranks.iter().position(|s| *s == want) else {
        return want.to_owned();
    };
    levels
        .iter()
        .filter_map(|s| {
            ranks
                .iter()
                .position(|rank| rank == s)
                .filter(|i| *i != 0)
                .map(|i| (i.abs_diff(at), std::cmp::Reverse(i), s))
        })
        .min_by_key(|(distance, rank, _)| (*distance, *rank))
        .map(|(_, _, s)| s.to_owned())
        .unwrap_or_else(|| want.to_owned())
}
pub(crate) fn request(chat: &Value, working_dir: Option<&str>, git: bool) -> PluginResult<Value> {
    let model = core::required(chat, "model")?;
    for key in [
        "top_p",
        "stop",
        "seed",
        "response_format",
        "logprobs",
        "top_logprobs",
        "frequency_penalty",
        "presence_penalty",
        "audio",
    ] {
        if !chat[key].is_null() {
            return Err(PluginError::invalid(format!(
                "Command Code's Go endpoint does not support {key}."
            )));
        }
    }
    if chat["n"].as_u64().is_some_and(|n| n != 1) {
        return Err(PluginError::invalid(
            "Command Code's Go endpoint supports one response per request.",
        ));
    }
    if !chat["tool_choice"].is_null() && chat["tool_choice"] != "auto" {
        return Err(PluginError::invalid(
            "Command Code's Go endpoint does not support an explicit tool choice.",
        ));
    }
    let mut system = Vec::new();
    let mut messages = Vec::new();
    let mut names = BTreeMap::new();
    let mut results = Vec::new();
    for message in chat["messages"]
        .as_array()
        .ok_or_else(|| PluginError::invalid("The Go request has no messages."))?
    {
        let role = message["role"].as_str().unwrap_or("");
        if role != "user"
            && message["content"]
                .as_array()
                .is_some_and(|parts| parts.iter().any(|part| part["type"] != "text"))
        {
            return Err(PluginError::invalid(
                "Command Code's Go endpoint supports text-only system, assistant and tool content.",
            ));
        }
        if message.get("function_call").is_some() {
            return Err(PluginError::invalid(
                "Use tool_calls for Command Code's Go endpoint.",
            ));
        }
        if ["system", "developer"].contains(&role) {
            system.push(text(&message["content"]));
            continue;
        }
        if role == "tool" {
            let id = core::required(message, "tool_call_id")?;
            results.push(json!({"type":"tool-result","toolCallId":id,"toolName":names.get(id).map(String::as_str).unwrap_or("unknown"),"output":{"type":"text","value":text(&message["content"])}}));
            continue;
        }
        if !results.is_empty() {
            messages.push(json!({"role":"tool","content":std::mem::take(&mut results)}));
        }
        let mut parts = Vec::new();
        if role == "assistant" {
            if let Some(thought) = message["reasoning_content"]
                .as_str()
                .or_else(|| message["reasoning"].as_str())
                .filter(|text| !text.is_empty())
            {
                parts.push(json!({"type":"reasoning","text":thought}));
            }
            let content = text(&message["content"]);
            if !content.is_empty() {
                parts.push(json!({"type":"text","text":content}));
            }
            for call in message["tool_calls"].as_array().into_iter().flatten() {
                let id = core::required(call, "id")?;
                let name = core::required(&call["function"], "name")?;
                let args = &call["function"]["arguments"];
                let input = if let Some(args) = args.as_str() {
                    serde_json::from_str::<Value>(args).map_err(|_| {
                        PluginError::invalid("A Go tool call contains invalid JSON arguments.")
                    })?
                } else if args.is_object() || args.is_array() {
                    args.clone()
                } else {
                    json!({})
                };
                names.insert(id.to_owned(), name.to_owned());
                parts.push(
                    json!({"type":"tool-call","toolCallId":id,"toolName":name,"input":input}),
                );
            }
        } else if role == "user" {
            if let Some(content) = message["content"].as_str() {
                if !content.is_empty() {
                    parts.push(json!({"type":"text","text":content}));
                }
            } else {
                for part in message["content"].as_array().into_iter().flatten() {
                    match part["type"].as_str() {
                        Some("text") => parts.push(json!({"type":"text","text":part["text"]})),
                        Some("image_url") => {
                            let image = core::text(&part["image_url"])
                                .or_else(|| core::text(&part["image_url"]["url"]))
                                .ok_or_else(|| PluginError::invalid("A Go image has no URL."))?;
                            let mut output = json!({"type":"image","image":image});
                            if let Some(data) = image.strip_prefix("data:") {
                                if let Some(end) = data.find([';', ',']) {
                                    output["mimeType"] = json!(&data[..end]);
                                }
                            }
                            parts.push(output);
                        }
                        _ => {
                            return Err(PluginError::invalid(
                                "Command Code's Go endpoint supports text and image input only.",
                            ))
                        }
                    }
                }
            }
        } else {
            return Err(PluginError::invalid(
                "The Go request has an unsupported message role.",
            ));
        }
        if !parts.is_empty() {
            messages.push(
                json!({"role":if role=="assistant"{"assistant"}else{"user"},"content":parts}),
            );
        }
    }
    if !results.is_empty() {
        messages.push(json!({"role":"tool","content":results}));
    }
    let tools=chat["tools"].as_array().into_iter().flatten().filter(|tool|tool["type"]=="function").map(|tool|{let function=&tool["function"];Ok(json!({"name":core::required(function,"name")?,"description":function["description"].as_str().unwrap_or(""),"input_schema":function.get("parameters").cloned().unwrap_or_else(||json!({"type":"object","properties":{}}))}))}).collect::<PluginResult<Vec<_>>>()?;
    let max =
        chat.get("max_completion_tokens")
            .or_else(|| chat.get("max_tokens"))
            .map(|value| {
                value.as_u64().filter(|n| *n > 0).ok_or_else(|| {
                    PluginError::invalid("Go max_tokens must be a positive integer.")
                })
            })
            .transpose()?
            .unwrap_or(64_000)
            .min(200_000);
    let mut params = json!({"model":model,"messages":messages,"tools":tools,"system":system.join("\n\n"),"max_tokens":max,"stream":true});
    if let Some(temperature) = chat["temperature"].as_f64() {
        params["temperature"] = json!(temperature);
    }
    if let Some(effort) = core::text(&chat["reasoning_effort"]).filter(|e| *e != "none") {
        params["reasoning_effort"] = json!(fit_effort(effort, &catalog::efforts(model)));
    }
    Ok(
        json!({"config":{"workingDir":working_dir.map(str::to_owned).or_else(||std::env::var("HOME").ok()).unwrap_or_default(),"date":chrono::Local::now().format("%Y-%m-%d").to_string(),"environment":if cfg!(target_os="macos"){"darwin"}else if cfg!(target_os="windows"){"win32"}else{"linux"},"structure":[],"isGitRepo":git,"currentBranch":"","mainBranch":"","gitStatus":"","recentCommits":[]},"memory":null,"taste":null,"skills":null,"permissionMode":"standard","params":params}),
    )
}
pub(crate) fn failure(status: u16, value: &Value) -> Value {
    let raw = value["error"]["message"]
        .as_str()
        .or_else(|| value["message"].as_str())
        .or_else(|| value["error"].as_str())
        .unwrap_or("Command Code could not complete this request.");
    let message: String = raw.chars().take(600).collect();
    let low = message.to_ascii_lowercase();
    let status = if low.contains("model_not_in_plan") {
        403
    } else if low.contains("premium_credits_exhausted") || low.contains("insufficient credits") {
        402
    } else if (status < 400 || status == 500)
        && (message.contains("RATE_LIMITED")
            || low.contains("usage limit")
            || low.contains("window_limit"))
    {
        429
    } else if !(400..600).contains(&status) {
        502
    } else {
        status
    };
    json!({"status":status,"message":message,"type":if value["type"]=="cmd_zdr_no_providers"||value["code"]=="CMD_ZDR_NO_PROVIDERS"{"cmd_zdr_no_providers"}else{"commandcode_error"}})
}
pub(crate) fn error_response(error: &Value) -> FetchResponse {
    FetchResponse::bytes(error["status"].as_u64().unwrap_or(502)as u16,BTreeMap::from([("content-type".into(),"application/json".into())]),serde_json::to_vec(&json!({"error":{"message":error["message"],"type":error["type"],"code":error["status"]}})).unwrap())
}
struct State {
    body: nora_ai::HttpByteStream,
    buffer: Vec<u8>,
    ended: bool,
    finished: bool,
    tools: u64,
    cache: u64,
    said: bool,
    wrote: bool,
}
fn event(line: &[u8], state: &mut State) -> Option<Value> {
    let line = line.strip_prefix(b"data:").unwrap_or(line);
    let value: Value = serde_json::from_slice(line).ok()?;
    match value["type"].as_str()? {
        "text-delta" => {
            let text = value["text"].as_str().filter(|text| !text.is_empty())?;
            state.said = true;
            state.wrote = true;
            Some(json!({"text":text}))
        }
        "reasoning-delta" => {
            let text = value["text"].as_str().filter(|text| !text.is_empty())?;
            state.said = true;
            Some(json!({"reasoning":text}))
        }
        "tool-call" => {
            if value["providerExecuted"] == true {
                return None;
            }
            let args = value
                .get("input")
                .or_else(|| value.get("args"))
                .cloned()
                .unwrap_or_else(|| json!({}));
            let args = if let Some(text) = args.as_str() {
                match serde_json::from_str::<Value>(text) {
                    Ok(args) => args,
                    Err(_) => {
                        state.finished = true;
                        return Some(
                            json!({"error":failure(502,&json!({"message":"Command Code returned invalid tool arguments."}))}),
                        );
                    }
                }
            } else {
                args
            };
            let name = match core::text(&value["toolName"]) {
                Some(name) => name,
                None => {
                    state.finished = true;
                    return Some(
                        json!({"error":failure(502,&json!({"message":"Command Code returned a tool call without a name."}))}),
                    );
                }
            };
            let id = core::text(&value["toolCallId"])
                .map(str::to_owned)
                .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4().simple()));
            let index = state.tools;
            state.tools += 1;
            state.said = true;
            Some(
                json!({"tool":{"index":index,"id":id,"name":name,"args":serde_json::to_string(&args).unwrap()}}),
            )
        }
        "cache-write-tokens" => {
            state.cache = state
                .cache
                .max(value["cacheWriteTokens"].as_u64().unwrap_or(0));
            None
        }
        "finish" => {
            state.finished = true;
            let total = &value["totalUsage"];
            let input = total["inputTokens"].as_u64().unwrap_or(0);
            let output = total["outputTokens"].as_u64().unwrap_or(0);
            let stop = match value["finishReason"].as_str() {
                Some("tool-calls") => "tool_calls",
                Some("length") => "length",
                Some("content-filter") => "content_filter",
                _ if state.tools > 0 => "tool_calls",
                _ => "stop",
            };
            Some(
                json!({"stop":stop,"usage":{"prompt_tokens":input,"completion_tokens":output,"total_tokens":input.saturating_add(output),"prompt_tokens_details":{"cached_tokens":total["inputTokenDetails"]["cacheReadTokens"].as_u64().unwrap_or(0),"cache_write_tokens":state.cache.max(total["inputTokenDetails"]["cacheWriteTokens"].as_u64().unwrap_or(0))},"completion_tokens_details":{"reasoning_tokens":total["outputTokenDetails"]["reasoningTokens"].as_u64().unwrap_or(0)}}}),
            )
        }
        "abort" => {
            state.finished = true;
            Some(json!({"stop":"stop"}))
        }
        "error" => {
            state.finished = true;
            let mut error = value["error"].clone();
            if error.is_string() {
                error = json!({"message":error});
            }
            let status = error["statusCode"].as_u64().unwrap_or(500) as u16;
            Some(json!({"error":failure(status,&error)}))
        }
        _ => None,
    }
}
pub(crate) fn events(body: nora_ai::HttpByteStream) -> BoxStream<'static, PluginResult<Value>> {
    let initial = State {
        body,
        buffer: Vec::new(),
        ended: false,
        finished: false,
        tools: 0,
        cache: 0,
        said: false,
        wrote: false,
    };
    Box::pin(futures::stream::unfold(initial, |mut state| async move {
        if state.finished {
            return None;
        }
        loop {
            if let Some(end) = state.buffer.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = state.buffer.drain(..=end).collect();
                if let Some(value) = event(&line, &mut state) {
                    return Some((Ok(value), state));
                }
                continue;
            }
            if state.ended {
                if !state.buffer.is_empty() {
                    let line = std::mem::take(&mut state.buffer);
                    if let Some(value) = event(&line, &mut state) {
                        return Some((Ok(value), state));
                    }
                }
                state.finished = true;
                let error = failure(
                    502,
                    &json!({"message":"The reply ended before it was complete."}),
                );
                return Some((
                    Ok(if state.said {
                        json!({"cut":if state.tools>0{"tool_calls"}else{"stop"},"wrote":state.wrote,"error":error})
                    } else {
                        json!({"error":error})
                    }),
                    state,
                ));
            }
            match tokio::time::timeout(Duration::from_secs(180), state.body.next()).await {
                Ok(Some(Ok(chunk))) => {
                    if chunk.len() > MAX_FRAME.saturating_sub(state.buffer.len()) {
                        state.finished = true;
                        return Some((
                            Err(PluginError::transport(
                                "A Command Code response frame exceeds 32 MiB.",
                            )),
                            state,
                        ));
                    }
                    state.buffer.extend_from_slice(&chunk);
                }
                Ok(None) => state.ended = true,
                _ => {
                    state.finished = true;
                    return Some((
                        Err(PluginError::transport("Command Code's reply stopped.")),
                        state,
                    ));
                }
            }
        }
    }))
}
fn chunk(
    id: &str,
    created: u64,
    model: &str,
    delta: Value,
    stop: Value,
    usage: Option<&Value>,
) -> Vec<u8> {
    let mut value = json!({"id":id,"object":"chat.completion.chunk","created":created,"model":model,"choices":[{"index":0,"delta":delta,"finish_reason":stop}]});
    if let Some(usage) = usage {
        value["usage"] = usage.clone();
    }
    format!("data: {}\n\n", serde_json::to_string(&value).unwrap()).into_bytes()
}
pub(crate) async fn response(
    response: nora_ai::HttpStreamingResponse,
    chat: &Value,
) -> PluginResult<FetchResponse> {
    if !(200..300).contains(&response.status) {
        let status = response.status;
        let bytes = core::collect(response, 1024 * 1024).await?;
        let value=serde_json::from_slice(&bytes).unwrap_or_else(|_|json!({"message":String::from_utf8_lossy(&bytes).chars().take(600).collect::<String>()}));
        return Ok(error_response(&failure(status, &value)));
    }
    let mut events = events(response.body);
    let first = events.next().await.transpose()?;
    if let Some(error) = first.as_ref().and_then(|value| value.get("error")) {
        return Ok(error_response(error));
    }
    let model = core::required(chat, "model")?.to_owned();
    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
    let created = core::now() / 1000;
    if chat["stream"] != true {
        let mut pending = first;
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut calls = Vec::new();
        let mut stop = json!("stop");
        let mut usage = None;
        let mut size = 0usize;
        loop {
            let value = if let Some(value) = pending.take() {
                Some(value)
            } else {
                events.next().await.transpose()?
            };
            let Some(value) = value else {
                break;
            };
            if let Some(error) = value.get("error") {
                return Ok(error_response(error));
            }
            size = size.saturating_add(value.to_string().len());
            if size > MAX_FRAME {
                return Err(PluginError::transport(
                    "Command Code's response exceeds 32 MiB.",
                ));
            }
            if let Some(text) = value["text"].as_str() {
                content.push_str(text);
            }
            if let Some(text) = value["reasoning"].as_str() {
                reasoning.push_str(text);
            }
            if let Some(tool) = value.get("tool") {
                calls.push(json!({"id":tool["id"],"type":"function","function":{"name":tool["name"],"arguments":tool["args"]}}));
            }
            if let Some(reason) = value.get("stop") {
                stop = reason.clone();
                usage = value.get("usage").cloned();
            }
        }
        let mut message = json!({"role":"assistant","content":content});
        if !reasoning.is_empty() {
            message["reasoning_content"] = json!(reasoning);
        }
        if !calls.is_empty() {
            message["tool_calls"] = json!(calls);
        }
        let mut value = json!({"id":id,"object":"chat.completion","created":created,"model":model,"choices":[{"index":0,"message":message,"finish_reason":stop}]});
        if let Some(usage) = usage {
            value["usage"] = usage;
        }
        return Ok(FetchResponse::bytes(
            200,
            BTreeMap::from([("content-type".into(), "application/json".into())]),
            serde_json::to_vec(&value).unwrap(),
        ));
    }
    let state = (events, first, false, false);
    let body = futures::stream::unfold(state, move |(mut events, mut pending, began, finished)| {
        let id = id.clone();
        let model = model.clone();
        async move {
            if finished {
                return None;
            }
            let next = if let Some(value) = pending.take() {
                Ok(Some(value))
            } else {
                events.next().await.transpose()
            };
            let value = match next {
                Ok(Some(value)) => value,
                Ok(None) => return None,
                Err(error) => return Some((Err(error), (events, None, true, true))),
            };
            let mut bytes = if began {
                Vec::new()
            } else {
                chunk(
                    &id,
                    created,
                    &model,
                    json!({"role":"assistant","content":""}),
                    Value::Null,
                    None,
                )
            };
            let mut done = false;
            if let Some(text) = value.get("text") {
                bytes.extend(chunk(
                    &id,
                    created,
                    &model,
                    json!({"content":text}),
                    Value::Null,
                    None,
                ));
            } else if let Some(text) = value.get("reasoning") {
                bytes.extend(chunk(
                    &id,
                    created,
                    &model,
                    json!({"reasoning_content":text}),
                    Value::Null,
                    None,
                ));
            } else if let Some(tool) = value.get("tool") {
                bytes.extend(chunk(&id,created,&model,json!({"tool_calls":[{"index":tool["index"],"id":tool["id"],"type":"function","function":{"name":tool["name"],"arguments":tool["args"]}}]}),Value::Null,None));
            } else if let Some(stop) = value.get("cut") {
                let note=format!("{}(The reply was cut short: Command Code closed the stream before it finished. Ask again to go on.)",if value["wrote"]==true{"\n\n"}else{""});
                bytes.extend(chunk(
                    &id,
                    created,
                    &model,
                    json!({"content":note}),
                    Value::Null,
                    None,
                ));
                bytes.extend(chunk(&id, created, &model, json!({}), stop.clone(), None));
                bytes.extend_from_slice(b"data: [DONE]\n\n");
                done = true;
            } else if let Some(error) = value.get("error") {
                bytes.extend(
                    format!(
                        "data: {}\n\n",
                        json!({"error":{"message":error["message"],"code":error["status"]}})
                    )
                    .into_bytes(),
                );
                done = true;
            } else if let Some(stop) = value.get("stop") {
                bytes.extend(chunk(
                    &id,
                    created,
                    &model,
                    json!({}),
                    stop.clone(),
                    value.get("usage"),
                ));
                bytes.extend_from_slice(b"data: [DONE]\n\n");
                done = true;
            }
            Some((Ok(bytes), (events, None, true, done)))
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
#[path = "../test/generate.test.rs"]
mod tests;
