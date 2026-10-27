use super::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct Learned {
    systems: BTreeSet<String>,
    after_tools: BTreeSet<String>,
}

pub(super) fn headers(site: Site, credentials: &Value) -> BTreeMap<String, String> {
    let mut headers = auth::enterprise_headers(credentials);
    headers.extend([
        (
            "authorization".into(),
            format!("Bearer {}", text(credentials, "access")),
        ),
        ("x-user-id".into(), text(credentials, "uid").into()),
        ("x-domain".into(), auth::domain(site, credentials)),
        ("x-product".into(), "SaaS".into()),
        ("x-ide-type".into(), "WorkBuddy".into()),
        ("user-agent".into(), "WorkBuddy/5.5.6".into()),
        ("x-requested-with".into(), "XMLHttpRequest".into()),
        ("x-agent-intent".into(), "craft".into()),
        ("x-agent-purpose".into(), "conversation".into()),
        ("x-ide-name".into(), "WorkBuddy".into()),
        ("x-ide-version".into(), "5.5.6".into()),
        ("content-type".into(), "application/json".into()),
        ("accept".into(), "application/json".into()),
    ]);
    if site == Site::International {
        headers.insert("x-agent-type".into(), "main".into());
    }
    headers
}

pub(super) async fn fetch(
    provider: &WorkBuddy,
    request: FetchRequest,
) -> PluginResult<FetchResponse> {
    if request.method != "POST"
        || request.url != format!("{}/chat/completions", provider.site.base())
    {
        return Err(invalid(
            "WorkBuddy only accepts its official chat endpoint.",
        ));
    }
    let client = provider.client(request.network_proxy.as_ref())?;
    let credentials = auth::current(
        provider,
        client.as_ref(),
        request.credentials.as_ref().ok_or_else(signed_out)?,
    )
    .await?;
    let bytes = request
        .decode_body()?
        .ok_or_else(|| invalid("WorkBuddy needs a chat request body."))?;
    let mut body: Value = serde_json::from_slice(&bytes)
        .map_err(|_| invalid("The WorkBuddy chat request is invalid JSON."))?;
    prepare(&mut body)?;
    let names = tools::fit(&mut body)?;
    tool_choice(&mut body);
    let mut headers = headers(provider.site, &credentials);
    attend(&mut headers, &body, &request.metadata, &provider.salt);
    let system = system_key(&body);
    let model = text(&body, "model").to_owned();
    {
        let learned = provider.learned.lock().await;
        if learned.systems.contains(&system) {
            system_as_user(&mut body);
        }
        if learned.after_tools.contains(&model) {
            after_tools(&mut body);
        }
    }
    let mut tried_system = false;
    let mut tried_after = false;
    loop {
        let bytes = body.to_string().into_bytes();
        let response = send(
            client.as_ref(),
            HttpRequest {
                method: HttpMethod::Post,
                url: request.url.clone(),
                headers: headers.clone(),
                body: Some(bytes.clone()),
            },
            Duration::from_secs(120),
        )
        .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                if bytes.len() > 2 * 1024 * 1024 {
                    if let Ok(probe) = send(
                        client.as_ref(),
                        HttpRequest {
                            method: HttpMethod::Post,
                            url: format!(
                                "{}/billing/meter/get-user-resource-summary",
                                provider.site.endpoint()
                            ),
                            headers: headers.clone(),
                            body: Some(b"{}".to_vec()),
                        },
                        Duration::from_secs(20),
                    )
                    .await
                    {
                        if probe.status == 401 {
                            return Ok(error_response(401,"authentication_error",401,"WorkBuddy refused this sign-in and closed the large request. Reconnect the account."));
                        }
                    }
                }
                return Err(error);
            }
        };
        if response.status < 400 {
            return Ok(stream::renamed(response, names));
        }
        let (status, mut response_headers, bytes) = super::bytes(response, 2 * 1024 * 1024).await?;
        let text = String::from_utf8_lossy(&bytes);
        let lowered = text.to_lowercase();
        if status == 400
            && !tried_system
            && !system.is_empty()
            && (lowered.contains("unapproved channel")
                || lowered.contains("illegal api invocation"))
        {
            tried_system = true;
            if system_as_user(&mut body) {
                let mut learned = provider.learned.lock().await;
                remember(&mut learned.systems, system.clone());
                continue;
            }
        }
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let code = error_code(&value);
        if status == 400
            && !tried_after
            && (code == Some(11153) || lowered.contains("must end with a user message"))
        {
            tried_after = true;
            if after_tools(&mut body) {
                let mut learned = provider.learned.lock().await;
                remember(&mut learned.after_tools, model.clone());
                continue;
            }
        }
        response_headers.retain(|name, _| {
            !matches!(
                name.to_ascii_lowercase().as_str(),
                "content-length" | "content-encoding"
            )
        });
        response_headers.insert("x-magpie-sign-in".into(), "kept".into());
        if code == Some(14018)
            || lowered.contains("credits exhausted")
            || lowered.contains("额度已用尽")
        {
            return Ok(error_response(status,"insufficient_quota",14018,"WorkBuddy requires an active credit package, including for models priced at zero. Check the account's package balance."));
        }
        if code == Some(11140)
            || lowered.contains("account has been restricted due to unusual activity")
        {
            return Ok(error_response(status,"permission_error",11140,"WorkBuddy restricted this account. Submit feedback in WorkBuddy to request a review."));
        }
        return Ok(FetchResponse::bytes(status, response_headers, bytes));
    }
}

fn remember(values: &mut BTreeSet<String>, key: String) {
    if values.len() >= 256 {
        if let Some(first) = values.first().cloned() {
            values.remove(&first);
        }
    }
    values.insert(key);
}
fn error_code(value: &Value) -> Option<i64> {
    ["/error/data/code", "/error/code", "/code", "/data/code"]
        .into_iter()
        .find_map(|path| {
            value
                .pointer(path)
                .and_then(number)
                .map(|value| value as i64)
        })
}
fn error_response(status: u16, kind: &str, code: i64, message: &str) -> FetchResponse {
    FetchResponse::bytes(
        status,
        BTreeMap::from([
            ("content-type".into(), "application/json".into()),
            ("x-magpie-sign-in".into(), "kept".into()),
        ]),
        json!({"error":{"type":kind,"code":code,"message":message}})
            .to_string()
            .into_bytes(),
    )
}

pub(super) fn prepare(body: &mut Value) -> PluginResult<()> {
    let messages = body
        .get_mut("messages")
        .and_then(Value::as_array_mut)
        .filter(|messages| !messages.is_empty())
        .ok_or_else(|| invalid("WorkBuddy needs at least one chat message."))?;
    if messages.len() > 16_384 {
        return Err(invalid("The WorkBuddy conversation has too many messages."));
    }
    for message in messages.iter_mut() {
        if text(message, "role") == "developer" {
            message["role"] = json!("system");
        }
        if let Some(content) = message.get_mut("content") {
            match content {
                Value::String(text) => *text = unflagged(text),
                Value::Array(parts) => {
                    for part in parts {
                        if let Some(Value::String(text)) = part.get_mut("text") {
                            *text = unflagged(text);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    messages.retain(|message| {
        text(message, "role") != "system"
            || !content_text(message.get("content").unwrap_or(&Value::Null))
                .trim()
                .is_empty()
    });
    if messages
        .first()
        .is_none_or(|message| text(message, "role") != "system")
    {
        messages.insert(
            0,
            json!({"role":"system","content":"You are a helpful assistant."}),
        );
    }
    if let Some(object) = body.as_object_mut() {
        if let Some(maximum) = object.remove("max_completion_tokens") {
            object.entry("max_tokens").or_insert(maximum);
        }
        object.insert("stream".into(), json!(true));
    }
    Ok(())
}

pub(super) fn unflagged(text: &str) -> String {
    use std::sync::OnceLock;
    static PATTERNS: OnceLock<Vec<(regex::Regex, &'static str)>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            (r"(?im)x-anthropic-billing-header:[^\n]*\n?", ""),
            (
                r"(?i)You are Claude Code, Anthropic['’]s official CLI for Claude",
                "You are Claude Code, Anthropic's CLI for Claude",
            ),
            (
                r"(?i)Main branch \(you will usually use this for PRs\)",
                "Main branch (usually the base for PRs)",
            ),
            (
                r"(?i)You are an AI agent powered by DeepSeek Harness\.?",
                "You are an AI agent that helps the user with programming tasks.",
            ),
        ]
        .into_iter()
        .map(|(pattern, replacement)| (regex::Regex::new(pattern).unwrap(), replacement))
        .collect()
    });
    let mut output = text.to_owned();
    for (regex, replacement) in patterns {
        output = regex.replace_all(&output, *replacement).into_owned();
    }
    static CODEX: OnceLock<regex::Regex> = OnceLock::new();
    CODEX
        .get_or_init(|| {
            regex::Regex::new(r"(?i)(the )?Codex CLI is an open source project led by OpenAI\.")
                .unwrap()
        })
        .replace_all(&output, |captures: &regex::Captures<'_>| {
            if captures.get(1).is_some() {
                captures[0].to_owned()
            } else {
                format!("The {}", &captures[0])
            }
        })
        .into_owned()
}
fn content_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}
fn system_key(body: &Value) -> String {
    let value = body
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|message| text(message, "role") == "system")
        .map(|message| content_text(message.get("content").unwrap_or(&Value::Null)))
        .collect::<Vec<_>>()
        .join("\n");
    if value.trim() == "You are a helpful assistant." {
        String::new()
    } else {
        format!("{:x}", Sha256::digest(value.as_bytes()))
    }
}
pub(super) fn system_as_user(body: &mut Value) -> bool {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return false;
    };
    let count = messages
        .iter()
        .take_while(|message| text(message, "role") == "system")
        .count();
    let leading = messages[..count]
        .iter()
        .map(|message| content_text(message.get("content").unwrap_or(&Value::Null)))
        .collect::<Vec<_>>()
        .join("\n\n");
    let moved = !leading.trim().is_empty() && leading.trim() != "You are a helpful assistant.";
    let has_later = messages[count..]
        .iter()
        .any(|message| text(message, "role") == "system");
    if !moved && !has_later {
        return false;
    }
    let mut rest = messages.split_off(count);
    for message in &mut rest {
        if text(message, "role") == "system" {
            message["role"] = json!("user");
        }
    }
    if moved {
        let instructions = format!("<instructions>\n{}\n</instructions>", leading.trim());
        if let Some(user) = rest
            .iter_mut()
            .find(|message| text(message, "role") == "user")
        {
            match user.get_mut("content") {
                Some(Value::String(text)) => *text = format!("{instructions}\n\n{text}"),
                Some(Value::Array(parts)) => {
                    parts.insert(0, json!({"type":"text","text":instructions}))
                }
                _ => user["content"] = json!(instructions),
            }
        } else {
            rest.insert(0, json!({"role":"user","content":instructions}));
        }
    }
    messages.clear();
    messages.push(json!({"role":"system","content":"You are a helpful assistant."}));
    messages.extend(rest);
    true
}
pub(super) fn after_tools(body: &mut Value) -> bool {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return false;
    };
    let count = messages
        .iter()
        .rev()
        .take_while(|message| text(message, "role") == "user")
        .count();
    let before = messages.len().saturating_sub(count);
    if count == 0 || before == 0 || text(&messages[before - 1], "role") != "tool" {
        return false;
    }
    messages.insert(before, json!({"role":"assistant","content":""}));
    true
}
fn tool_choice(body: &mut Value) {
    let Some(choice) = body.get("tool_choice").cloned() else {
        return;
    };
    let kind = choice.as_str().unwrap_or_else(|| text(&choice, "type"));
    match kind {
        "none" => {
            let object = body.as_object_mut().unwrap();
            for key in ["tool_choice", "tools", "functions"] {
                object.remove(key);
            }
        }
        "auto" | "required" => body["tool_choice"] = json!(kind),
        "function" => {
            body["tool_choice"] = json!(choice
                .pointer("/function/name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .unwrap_or("auto"))
        }
        _ => {
            body.as_object_mut().unwrap().remove("tool_choice");
        }
    }
}
fn signature(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| match text(part, "type") {
                "" | "text" => Some(text(part, "text").to_owned()),
                "tool_result" | "tool-result" => None,
                kind => Some(format!(
                    "[{kind}:{:x}]",
                    Sha256::digest({
                        let mut part = part.clone();
                        part.sort_all_objects();
                        part.to_string()
                    }.as_bytes())
                )),
            })
            .collect(),
        _ => String::new(),
    }
}
fn attend(
    headers: &mut BTreeMap<String, String>,
    body: &Value,
    metadata: &BTreeMap<String, String>,
    salt: &str,
) {
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let conversation = metadata
        .get("sessionID")
        .filter(|session| !session.is_empty())
        .map(|session| format!("session:{session}"))
        .or_else(|| {
            messages
                .iter()
                .find(|message| text(message, "role") == "user")
                .map(|message| {
                    format!(
                        "first:{}",
                        signature(message.get("content").unwrap_or(&Value::Null))
                    )
                })
        });
    let turn = metadata
        .get("turnID")
        .filter(|turn| !turn.is_empty())
        .cloned()
        .or_else(|| {
            messages
                .iter()
                .enumerate()
                .rev()
                .find_map(|(index, message)| {
                    if text(message, "role") != "user"
                        || index > 0
                            && (text(&messages[index - 1], "role") == "tool"
                                || messages[index - 1]
                                    .get("tool_calls")
                                    .and_then(Value::as_array)
                                    .is_some_and(|calls| !calls.is_empty()))
                    {
                        return None;
                    }
                    let value = signature(message.get("content").unwrap_or(&Value::Null));
                    (!value.is_empty()).then(|| format!("u{index}:{value}"))
                })
        });
    let id = |kind: &str, value: &str| {
        format!(
            "{:x}",
            Sha256::digest(format!("{salt}|{kind}|{value}").as_bytes())
        )[..32]
            .to_owned()
    };
    if let Some(conversation) = &conversation {
        headers.insert("x-conversation-id".into(), id("conversation", conversation));
    }
    headers.insert(
        "x-conversation-request-id".into(),
        turn.map(|turn| {
            id(
                "turn",
                &format!("{}|{turn}", conversation.unwrap_or_default()),
            )
        })
        .unwrap_or_else(random_id),
    );
    let message = random_id();
    headers.insert("x-conversation-message-id".into(), message.clone());
    headers.insert("x-request-id".into(), message);
}

#[cfg(test)]
#[path = "../test/request.test.rs"]
mod tests;

#[cfg(test)]
#[path="../test/request_identity.test.rs"]
mod identity_tests;
