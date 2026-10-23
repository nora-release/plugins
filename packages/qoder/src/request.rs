use crate::*;
use serde_json::Map;
use std::collections::BTreeSet;
const SYSTEM:&str="You are a Qoder agent. Use the instructions below and the tools available to you to assist the user.";
pub(crate) async fn fetch(provider: &Qoder, request: FetchRequest) -> PluginResult<FetchResponse> {
    let credentials = checked(request.credentials.as_ref().ok_or_else(signed_out)?)?;
    let deployment = provider.deployment(&credentials)?;
    let public = provider.site.deployment(None)?;
    if request.method != "POST"
        || ![
            format!("{}/chat/completions", deployment.api),
            format!("{}/chat/completions", public.api),
        ]
        .contains(&request.url)
    {
        return Err(invalid("Qoder only accepts its official chat endpoint."));
    }
    let bytes = request
        .decode_body()?
        .ok_or_else(|| invalid("Qoder needs a chat request body."))?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err(invalid("The Qoder request exceeds 32 MiB."));
    }
    let chat: Value = serde_json::from_slice(&bytes)
        .map_err(|_| invalid("The Qoder chat request is invalid JSON."))?;
    let model = text(&chat, "model");
    if model.is_empty() || !chat.get("messages").is_some_and(Value::is_array) {
        return Err(invalid("Qoder needs a model and chat messages."));
    }
    let client = provider.client(request.network_proxy.as_ref())?;
    let credentials = auth::fresh(provider, client.as_ref(), &credentials).await?;
    let entries = provider
        .listing(client.as_ref(), &credentials, false)
        .await?;
    let entry = if let Some(entry) = entries.into_iter().find(|entry| entry.key == model) {
        entry
    } else {
        provider
            .listing(client.as_ref(), &credentials, true)
            .await?
            .into_iter()
            .find(|entry| entry.key == model)
            .ok_or_else(|| invalid(format!("Qoder does not enable the model {model}.")))?
    };
    let body = body(&chat, &entry)?;
    let wire = codec::encode(&body.to_string());
    let url = format!("{}{CHAT_PATH}", deployment.api);
    let mut headers = codec::headers(&url, &credentials, &wire)?;
    headers.insert("accept".into(), "text/event-stream".into());
    headers.insert("cache-control".into(), "no-cache".into());
    headers.insert("x-model-key".into(), entry.key);
    headers.insert("x-model-source".into(), entry.source);
    let response = send(
        client.as_ref(),
        HttpRequest {
            method: HttpMethod::Post,
            url,
            headers,
            body: Some(wire.into_bytes()),
        },
        120,
    )
    .await?;
    if !(200..300).contains(&response.status) {
        let (status, _, bytes) = read(response, 1024 * 1024).await?;
        return Ok(response::failure(status, &String::from_utf8_lossy(&bytes)).response());
    }
    response::answer(response, &chat).await
}
fn blocks(value: &Value) -> Vec<Value> {
    match value {
        Value::String(text) if !text.is_empty() => vec![json!({"type":"text","text":text})],
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| match text(part, "type") {
                "text" if part.get("text").is_some() => {
                    Some(json!({"type":"text","text":part.get("text")}))
                }
                "image_url" => image(part),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}
fn image(part: &Value) -> Option<Value> {
    let value = part.get("image_url")?;
    let url = value
        .as_str()
        .or_else(|| value.get("url").and_then(Value::as_str))?;
    (!url.is_empty()).then(|| json!({"type":"image_url","image_url":{"url":url}}))
}
fn text_of(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.into();
    }
    blocks(value)
        .iter()
        .filter(|part| text(part, "type") == "text")
        .map(|part| text(part, "text"))
        .collect()
}
fn flush(out: &mut Vec<Value>, seen: &mut Vec<Value>) {
    if !seen.is_empty() {
        out.push(json!({"role":"user","content":std::mem::take(seen)}));
    }
}
struct Turn {
    open: Vec<String>,
    kept: BTreeSet<String>,
}
pub(crate) fn messages(messages: &[Value]) -> PluginResult<Vec<Value>> {
    if messages.len() > 100_000 {
        return Err(invalid("The Qoder conversation has too many messages."));
    }
    let mut out = Vec::new();
    let mut names = BTreeMap::new();
    let mut seen = Vec::new();
    let mut turn: Option<Turn> = None;
    for (index, message) in messages.iter().enumerate() {
        let role = text(message, "role");
        if ["system", "developer"].contains(&role) {
            continue;
        }
        if role == "user" {
            turn = None;
        }
        if role != "user" && role != "tool" {
            flush(&mut out, &mut seen);
        }
        if role == "tool" {
            let mut said = text_of(message.get("content").unwrap_or(&Value::Null));
            let images = blocks(message.get("content").unwrap_or(&Value::Null))
                .into_iter()
                .filter(|part| text(part, "type") == "image_url")
                .collect::<Vec<_>>();
            let named = text(message, "tool_call_id").trim();
            let pick = turn.as_ref().and_then(|turn| {
                turn.open
                    .iter()
                    .position(|id| id == named)
                    .or_else(|| turn.open.iter().position(|id| !turn.kept.contains(id)))
            });
            let Some(pick) = pick else {
                if !said.trim().is_empty() || !images.is_empty() {
                    if !seen.is_empty() {
                        seen.push(json!({"type":"text","text":"\n\n"}));
                    }
                    if !said.trim().is_empty() {
                        seen.push(json!({"type":"text","text":said.trim()}));
                    }
                    seen.extend(images);
                }
                continue;
            };
            let answered = turn.as_mut().unwrap().open.remove(pick);
            if !images.is_empty() {
                let name = names
                    .get(&answered)
                    .map(String::as_str)
                    .unwrap_or_else(|| text(message, "name"));
                let description = if name.is_empty() {
                    format!("tool call {answered}")
                } else {
                    format!("{name} (tool call {answered})")
                };
                seen.push(
                    json!({"type":"text","text":format!("[From the result of {description}:]")}),
                );
                let note = if images.len() == 1 {
                    "[The tool returned an image; it follows in the next message.]".into()
                } else {
                    format!(
                        "[The tool returned {} images; they follow in the next message.]",
                        images.len()
                    )
                };
                seen.extend(images);
                if !said.trim().is_empty() {
                    said.push_str("\n\n");
                }
                said.push_str(&note);
            }
            out.push(json!({"role":"tool","tool_call_id":answered,"content":said}));
            continue;
        }
        if role == "assistant" {
            if let Some(calls) = message
                .get("tool_calls")
                .and_then(Value::as_array)
                .filter(|calls| !calls.is_empty())
            {
                if calls.len() > 1024 {
                    return Err(invalid("The Qoder conversation has too many tool calls."));
                }
                let mut normalized = Vec::new();
                let mut open = Vec::new();
                for call in calls {
                    let call_id = if text(call, "id").trim().is_empty() {
                        format!("call_{}", id())
                    } else {
                        text(call, "id").trim().into()
                    };
                    let function = call.get("function").unwrap_or(&Value::Null);
                    if text(function, "name").is_empty() {
                        return Err(invalid("A Qoder tool call has no function name."));
                    }
                    names.insert(call_id.clone(), text(function, "name").to_owned());
                    open.push(call_id.clone());
                    let arguments = function
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    normalized.push(json!({"id":call_id,"type":"function","function":{"name":function.get("name"),"arguments":arguments.as_str().map(str::to_owned).unwrap_or_else(||arguments.to_string())}}));
                }
                out.push(json!({"role":"assistant","content":text_of(message.get("content").unwrap_or(&Value::Null)),"tool_calls":normalized}));
                let kept = messages[index + 1..]
                    .iter()
                    .take_while(|message| text(message, "role") == "tool")
                    .map(|message| text(message, "tool_call_id").to_owned())
                    .filter(|id| open.contains(id))
                    .collect();
                turn = Some(Turn { open, kept });
                continue;
            }
        }
        let mut parts = blocks(message.get("content").unwrap_or(&Value::Null));
        if role == "assistant" {
            parts.retain(|part| text(part, "type") == "text");
        }
        if parts.is_empty() {
            continue;
        }
        if role == "user" && !seen.is_empty() {
            let mut merged = std::mem::take(&mut seen);
            merged.extend(parts);
            parts = merged;
        }
        out.push(json!({"role":role,"content":parts}));
        turn = None;
    }
    flush(&mut out, &mut seen);
    Ok(out)
}
pub(crate) fn window(chat: &Value, entry: &models::Entry, reply: u64) -> u64 {
    if entry.windows.is_empty() {
        return entry.context;
    }
    let base = if entry.default_window > 0 {
        entry.default_window
    } else {
        entry
            .config
            .get("max_input_tokens")
            .and_then(Value::as_u64)
            .filter(|value| *value > 0)
            .unwrap_or(entry.context)
    };
    let size=json!({"m":chat.get("messages").cloned().unwrap_or(json!([])),"t":chat.get("tools").cloned().unwrap_or(json!([]))}).to_string().len();
    let need = (size as u64).div_ceil(3).saturating_add(reply);
    if base > 0 && need <= base {
        return base;
    }
    entry
        .windows
        .iter()
        .copied()
        .find(|window| *window >= need)
        .unwrap_or(entry.context)
}
pub(crate) fn body(chat: &Value, entry: &models::Entry) -> PluginResult<Value> {
    let messages = chat
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("Qoder needs chat messages."))?;
    let system = messages
        .iter()
        .filter(|message| ["system", "developer"].contains(&text(message, "role")))
        .map(|message| text_of(message.get("content").unwrap_or(&Value::Null)))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let tools=chat.get("tools").and_then(Value::as_array).into_iter().flatten().filter(|tool|text(chat,"tool_choice")!="none" && text(tool,"type")=="function" && !text(&tool["function"],"name").is_empty()).map(|tool|json!({"type":"function","function":{"name":tool["function"]["name"],"description":tool["function"].get("description").cloned().unwrap_or(json!("")),"parameters":tool["function"].get("parameters").cloned().unwrap_or(json!({"type":"object","properties":{}}))}})).collect::<Vec<_>>();
    let mut system = if system.is_empty() {
        SYSTEM.into()
    } else {
        format!("{SYSTEM}\n\n{system}")
    };
    if text(chat, "tool_choice") == "required" && !tools.is_empty() {
        system.push_str("\nYou must call an available function in this response.");
    }
    let system = json!({"type":"text","text":system});
    let (thinking, effort) = models::effort(text(chat, "reasoning_effort"), entry);
    let reply = chat
        .get("max_completion_tokens")
        .or_else(|| chat.get("max_tokens"))
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .unwrap_or(32000);
    let mut parameters = Map::from_iter([
        ("enable_thinking".into(), json!(thinking)),
        ("max_tokens".into(), json!(reply)),
    ]);
    if !effort.is_empty() {
        parameters.insert("reasoning_effort".into(), json!(effort));
    }
    let window = window(chat, entry, reply);
    if window > 0 {
        parameters.insert("context_length".into(), json!(window));
    }
    let mut turns = vec![json!({"role":"system","content":[system.clone()]})];
    turns.extend(self::messages(messages)?);
    let mut body = json!({"parameters":parameters,"business":{"product":"app","version":VERSION,"type":"agent","id":id(),"name":"Nora session","begin_at":now(),"stage":"start"},"agent_id":"agent_common","task_id":"common","session_type":"app","model_config":entry.config,"system":[system],"messages":turns});
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    Ok(body)
}
#[cfg(test)]
#[path = "../test/request.test.rs"]
mod tests;
