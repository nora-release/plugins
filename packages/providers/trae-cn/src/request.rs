use crate::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
pub(crate) async fn fetch(provider: &Trae, request: FetchRequest) -> PluginResult<FetchResponse> {
    let credentials = credentials(request.credentials.as_ref().ok_or_else(signed_out)?)?;
    let api = provider.site.api(&credentials);
    let public = provider.site.api(&json!({}));
    if request.method != "POST"
        || ![
            format!("{api}/v1/chat/completions"),
            format!("{public}/v1/chat/completions"),
        ]
        .contains(&request.url)
    {
        return Err(invalid("Trae only accepts its official chat endpoint."));
    }
    let bytes = request
        .decode_body()?
        .ok_or_else(|| invalid("Trae needs a chat request body."))?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err(invalid("The Trae request exceeds 32 MiB."));
    }
    let chat: Value = serde_json::from_slice(&bytes)
        .map_err(|_| invalid("The Trae chat request is invalid JSON."))?;
    if !chat.get("messages").is_some_and(Value::is_array) || text(&chat, "model").is_empty() {
        return Err(invalid("Trae needs a model and chat messages."));
    }
    let client = provider.client(request.network_proxy.as_ref())?;
    let entries = models::fetch(provider, client.as_ref(), &credentials, false).await?;
    let named = if let Some(entry) = entries
        .into_iter()
        .find(|entry| entry.id == text(&chat, "model"))
    {
        entry
    } else {
        models::fetch(provider, client.as_ref(), &credentials, true)
            .await?
            .into_iter()
            .find(|entry| entry.id == text(&chat, "model"))
            .ok_or_else(|| invalid("This Trae account no longer offers the selected model."))?
    };
    let account = provider.account(&credentials).await?;
    let last = account.lock().await.last_function.clone();
    let mut functions = vec![named.function.clone()];
    functions.extend(last);
    functions.extend(site::FUNCTIONS.into_iter().map(str::to_owned));
    let mut seen = BTreeSet::new();
    functions.retain(|function| seen.insert(function.clone()));
    let session = conversation(
        provider,
        &chat,
        request
            .metadata
            .get("sessionID")
            .map(String::as_str)
            .unwrap_or(""),
        &credentials,
    );
    let mut last = response::Failure::new(provider.site, 502, "", "Trae returned no answer.");
    for function in functions {
        let body = body(&chat, &function, &named, &session)?;
        let mut headers = site::ide_headers(&credentials);
        headers.insert("accept".into(), "text/event-stream".into());
        let response = tokio::time::timeout(
            Duration::from_secs(120),
            client.stream(HttpRequest {
                method: HttpMethod::Post,
                url: format!("{api}/api/agent/v3/llm_utils_chat"),
                headers,
                body: Some(body.to_string().into_bytes()),
            }),
        )
        .await
        .map_err(|_| transport("The Trae chat request timed out."))?
        .map_err(|_| transport("The Trae model service could not be reached."))?;
        let status = response.status;
        let json = response.headers.iter().any(|(key, value)| {
            key.eq_ignore_ascii_case("content-type")
                && value.contains("json")
                && !value.contains("event-stream")
        });
        if !(200..300).contains(&status) || json {
            let raw = tokio::time::timeout(
                Duration::from_secs(30),
                nora_ai::collect_http_body_text_limited(response.body, 1024 * 1024),
            )
            .await
            .map_err(|_| transport("The Trae error response timed out."))?
            .map_err(|_| transport("Trae returned an unreadable error response."))?;
            let value: Value =
                serde_json::from_str(&raw).unwrap_or(json!({"message":format!("HTTP {status}")}));
            let (code, message) = error_of(&value);
            last = response::Failure::new(
                provider.site,
                if json && status < 400 { 502 } else { status },
                &code,
                &message,
            );
            if wrong_function(&code) && (status == 400 || json) {
                continue;
            }
            break;
        }
        match response::prepare(response, provider.site, &chat).await? {
            Ok(response) => {
                account.lock().await.last_function = Some(function);
                return Ok(response);
            }
            Err(error) => {
                let wrong = wrong_function(&error.code);
                last = error;
                if wrong {
                    continue;
                }
                break;
            }
        }
    }
    Ok(last.response())
}
fn wrong_function(code: &str) -> bool {
    ["4001", "4023", "1005"].contains(&code)
}
fn text_of(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.into();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| {
            if let Some(text) = part.as_str() {
                Some(text.to_owned())
            } else if ["text", "input_text"].contains(&text(part, "type")) {
                Some(text(part, "text").to_owned())
            } else if text(part, "type") == "image_url" {
                Some("[an image, which this model can't see]".into())
            } else {
                None
            }
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
fn parts(content: &Value, sees: bool) -> (String, Vec<Value>) {
    if !sees {
        return (text_of(content), Vec::new());
    }
    let Some(parts) = content.as_array() else {
        return (text_of(content), Vec::new());
    };
    let mut images = Vec::new();
    let mut others = Vec::new();
    for part in parts {
        let image = if ["image_url", "input_image"].contains(&text(part, "type")) {
            part.get("image_url")
                .and_then(|image| {
                    image
                        .as_str()
                        .or_else(|| image.get("url").and_then(Value::as_str))
                })
                .or_else(|| part.get("url").and_then(Value::as_str))
        } else {
            None
        };
        if let Some(url) = image.filter(|url| !url.is_empty()) {
            images.push(json!({"type":"image_url","image_url":{"url":url}}));
        } else {
            others.push(part.clone());
        }
    }
    (text_of(&json!(others)), images)
}
#[derive(Default)]
struct Turn {
    role: String,
    text: String,
    images: Vec<Value>,
    calls: Vec<Value>,
    tool_id: String,
}
fn flush_images(out: &mut Vec<Turn>, images: &mut Vec<Value>) {
    if !images.is_empty() {
        out.push(Turn {
            role: "user".into(),
            images: std::mem::take(images),
            ..Turn::default()
        });
    }
}
pub(crate) fn messages(chat: &Value, sees: bool) -> PluginResult<Vec<Value>> {
    let native = chat
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty())
        && text(chat, "tool_choice") != "none";
    let mut names = BTreeMap::new();
    let mut dropped = BTreeSet::new();
    let mut out = Vec::new();
    let mut pending = Vec::new();
    let messages = chat
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("Trae needs chat messages."))?;
    if messages.len() > 100_000 {
        return Err(invalid("The Trae conversation has too many messages."));
    }
    for message in messages {
        let role = if text(message, "role") == "developer" {
            "system"
        } else {
            text(message, "role")
        };
        let (mut said, images) = parts(message.get("content").unwrap_or(&Value::Null), sees);
        if role == "assistant" {
            if let Some(calls) = message
                .get("tool_calls")
                .and_then(Value::as_array)
                .filter(|calls| !calls.is_empty())
            {
                if calls.len() > 1024 {
                    return Err(invalid("The Trae conversation has too many tool calls."));
                }
                let mut output = Vec::new();
                for call in calls {
                    let function = call.get("function").unwrap_or(call);
                    let name = if text(function, "name").is_empty() {
                        text(call, "name")
                    } else {
                        text(function, "name")
                    }
                    .trim();
                    let call_id = if text(call, "id").is_empty() {
                        format!("call_{}", id())
                    } else {
                        text(call, "id").into()
                    };
                    if name.is_empty() {
                        if !text(call, "id").is_empty() {
                            dropped.insert(text(call, "id").to_owned());
                        }
                        continue;
                    }
                    names.insert(call_id.clone(), name.to_owned());
                    let args = function
                        .get("arguments")
                        .or_else(|| call.get("arguments"))
                        .cloned()
                        .unwrap_or(json!({}));
                    let arguments = args
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| args.to_string());
                    if native {
                        output.push(json!({"id":call_id,"type":"function","function_call":{"name":name,"arguments":arguments}}));
                    } else {
                        let args =
                            serde_json::from_str::<Value>(&arguments).unwrap_or(json!(arguments));
                        if !said.is_empty() {
                            said.push('\n');
                        }
                        said.push_str(&format!(
                            "<tool_call>{}</tool_call>",
                            json!({"name":name,"arguments":args})
                        ));
                    }
                }
                if !output.is_empty() || !said.is_empty() || !images.is_empty() {
                    out.push(Turn {
                        role: role.into(),
                        text: said,
                        images,
                        calls: output,
                        ..Turn::default()
                    });
                }
                continue;
            }
        }
        if role == "tool" {
            let call_id = text(message, "tool_call_id");
            if dropped.contains(call_id) {
                continue;
            }
            if !native || !names.contains_key(call_id) {
                flush_images(&mut out, &mut pending);
                out.push(Turn {
                    role: "user".into(),
                    text: format!(
                        "Result of {} (call {}):\n{said}",
                        names.get(call_id).map(String::as_str).unwrap_or_else(|| {
                            if text(message, "name").is_empty() {
                                "the tool"
                            } else {
                                text(message, "name")
                            }
                        }),
                        if call_id.is_empty() { "?" } else { call_id }
                    ),
                    images,
                    ..Turn::default()
                });
                continue;
            }
            let has_images = !images.is_empty();
            pending.extend(images);
            out.push(Turn {
                role: "tool".into(),
                tool_id: call_id.into(),
                text: if said.is_empty() {
                    if has_images {
                        "(the result's image follows)"
                    } else {
                        "(no output)"
                    }
                    .into()
                } else {
                    said
                },
                ..Turn::default()
            });
            continue;
        }
        flush_images(&mut out, &mut pending);
        out.push(Turn {
            role: if ["assistant", "system"].contains(&role) {
                role
            } else {
                "user"
            }
            .into(),
            text: said,
            images,
            ..Turn::default()
        });
    }
    flush_images(&mut out, &mut pending);
    let mut merged: Vec<Turn> = Vec::new();
    for mut message in out {
        if message.role == "tool" {
            merged.push(message);
            continue;
        }
        if message.text.is_empty() && message.images.is_empty() && message.calls.is_empty() {
            continue;
        }
        if let Some(last) = merged
            .last_mut()
            .filter(|last| last.role == message.role && message.role != "system")
        {
            if !last.text.is_empty() && !message.text.is_empty() {
                last.text.push_str("\n\n");
            }
            last.text.push_str(&message.text);
            last.images.append(&mut message.images);
            last.calls.append(&mut message.calls);
        } else {
            merged.push(message);
        }
    }
    Ok(merged.into_iter().map(|message|{let mut content=Vec::new();if !message.text.is_empty(){content.push(json!({"type":"text","text":message.text}));}content.extend(message.images);if !message.calls.is_empty(){json!({"role":"assistant","content":if content.is_empty(){Value::Null}else{json!(content)},"tool_calls":message.calls})}else if message.role=="tool"{json!({"role":"tool","tool_call_id":message.tool_id,"content":content})}else{json!({"role":message.role,"content":content})}}).collect())
}
pub(crate) fn conversation(
    provider: &Trae,
    chat: &Value,
    session: &str,
    credentials: &Value,
) -> String {
    let who = format!(
        "{}|{}",
        provider.site.api(credentials),
        text(credentials, "uid")
    );
    let seed = if !session.trim().is_empty() {
        format!(
            "session|{who}|{}",
            session.chars().take(128).collect::<String>()
        )
    } else {
        let first = chat
            .get("messages")
            .and_then(Value::as_array)
            .and_then(|messages| {
                messages
                    .iter()
                    .find(|message| text(message, "role") == "user")
            })
            .map(|message| text_of(message.get("content").unwrap_or(&Value::Null)))
            .unwrap_or_default();
        if first.trim().is_empty() {
            return uuid::Uuid::new_v4().to_string();
        }
        format!("first|{who}|{first}")
    };
    let digest = Sha256::digest(format!("{}|{seed}", provider.salt).as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    uuid::Uuid::from_bytes(bytes).to_string()
}
pub(crate) fn body(
    chat: &Value,
    function: &str,
    entry: &models::Entry,
    session: &str,
) -> PluginResult<Value> {
    let config = entry
        .max
        .as_ref()
        .map(|(base, _)| base.as_str())
        .unwrap_or_else(|| text(chat, "model"));
    let use_named = entry.function == function || entry.max.is_some();
    let mut body = json!({"messages":messages(chat,entry.sees)?,"function":function,"config_name":config,"model":config,"stream":true,"request_id":uuid::Uuid::new_v4().to_string(),"session_id":session});
    if use_named && !entry.model_name.is_empty() {
        body["model_name"] = json!(entry.model_name);
    }
    let requested_effort = chat
        .get("reasoning_effort")
        .filter(|value| !value.is_null())
        .or_else(|| chat.get("reasoningEffort"));
    if let Some(effort) = entry.effort(requested_effort) {
        body["reasoning_effort"] = json!(effort);
    }
    let most = if use_named { entry.most } else { 0 };
    let asked = chat
        .get("max_completion_tokens")
        .or_else(|| chat.get("max_tokens"))
        .and_then(number)
        .filter(|asked| *asked > 0.0)
        .map(|asked| asked.floor() as u64);
    if let Some(asked) = asked {
        body["max_tokens"] = json!(if most > 0 { asked.min(most) } else { asked });
    }
    if let Some((_, window)) = entry.max.as_ref() {
        let window = *window;
        if body.get("max_tokens").is_none() && most > 0 {
            body["max_tokens"] = json!(most);
        }
        let output = body.get("max_tokens").and_then(Value::as_u64).unwrap_or(0);
        if output >= window {
            return Err(invalid(
                "The requested Trae reply leaves no room for the conversation.",
            ));
        }
        body["user_message_context"] = json!({"model_info":{"prompt_max_tokens":window-output}});
    }
    if let Some(temperature) = chat.get("temperature").filter(|value| value.is_number()) {
        body["temperature"] = temperature.clone();
    }
    if let Some(tools) = chat
        .get("tools")
        .and_then(Value::as_array)
        .filter(|tools| !tools.is_empty() && text(chat, "tool_choice") != "none")
    {
        body["tools"]=json!(tools.iter().map(|tool|{let function=tool.get("function").unwrap_or(tool);json!({"type":"function","function":{"name":function.get("name"),"description":function.get("description").cloned().unwrap_or(json!("")),"parameters":function.get("parameters").map(|value|value.as_str().map(str::to_owned).unwrap_or_else(||value.to_string())).unwrap_or_else(||"{}".into())}})}).collect::<Vec<_>>());
        if let Some(choice) = chat.get("tool_choice") {
            body["tool_choice"] = if choice.is_object() {
                json!(choice
                    .pointer("/function/name")
                    .or_else(|| choice.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("auto"))
            } else {
                choice.clone()
            };
        }
        if let Some(parallel) = chat
            .get("parallel_tool_calls")
            .filter(|value| value.is_boolean())
        {
            body["parallel_tool_calls"] = parallel.clone();
        }
    }
    Ok(body)
}
#[cfg(test)]
#[path = "../test/request.test.rs"]
mod tests;

#[cfg(test)]
#[path = "../test/reasoning.test.rs"]
mod reasoning_tests;
