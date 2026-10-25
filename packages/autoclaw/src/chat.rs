use super::*;
use regex::Regex;
use std::sync::LazyLock;

pub(super) const PREFIX: &str = "You are AutoClaw. Answer the user directly and concisely.";
const HINT:&str="AutoClaw refused this agent's identity instructions. Choose another provider for this request.";
static FLAGS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (
            r"You are a Claude agent, built on Anthropic's Claude Agent SDK\.?",
            "You are a coding assistant.",
        ),
        (
            r"(?:Anthropic's |the )?Claude Agent SDK",
            "the agent framework",
        ),
        (
            r"You are an AI agent powered by DeepSeek Harness",
            "You are an AI agent powered by DeepSeek-Harness",
        ),
        (r"You are Claude Code", "You are the Claude Code CLI"),
        (r"You are Codex", "You are the Codex agent"),
        (r"Codex CLI", "Codex command-line tool"),
        (r"You are opencode", "You are the opencode agent"),
        (r"You are Cursor", "You are the Cursor agent"),
        (r"You are Cline", "You are the Cline agent"),
        (r"You are Qwen Code", "You are the Qwen Code agent"),
        (r"You are ZCode", "You are the ZCode agent"),
        (
            r"operating inside pi, a coding agent harness",
            "operating inside the pi coding agent harness",
        ),
    ]
    .into_iter()
    .map(|(pattern, to)| {
        (
            Regex::new(&format!("(?i){pattern}")).expect("Static AutoClaw identity pattern"),
            to,
        )
    })
    .collect()
});
fn unflag(text: &str) -> String {
    FLAGS.iter().fold(text.to_owned(), |value, (pattern, to)| {
        pattern.replace_all(&value, *to).into_owned()
    })
}
pub(super) fn prepare(mut body: Value, site: Site) -> PluginResult<Value> {
    let object = body
        .as_object_mut()
        .ok_or_else(|| invalid("AutoClaw needs a JSON chat request."))?;
    object.remove("max_tokens");
    object.remove("max_completion_tokens");
    let messages = object
        .get_mut("messages")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| invalid("AutoClaw needs chat messages."))?;
    for message in messages
        .iter_mut()
        .filter(|m| matches!(m["role"].as_str(), Some("system" | "developer")))
    {
        match message.get_mut("content") {
            Some(Value::String(content)) => *content = unflag(content),
            Some(Value::Array(parts)) => {
                for part in parts {
                    if let Some(Value::String(content)) = part.get_mut("text") {
                        *content = unflag(content);
                    }
                }
            }
            _ => {}
        }
    }
    let at = if site == Site::China {
        messages
            .first()
            .filter(|m| m["role"] == "system")
            .map(|_| 0)
    } else {
        messages.iter().position(|m| m["role"] == "system")
    };
    if let Some(at) = at {
        let content = &mut messages[at]["content"];
        match content {
            Value::String(value) => {
                if !value.starts_with(PREFIX) {
                    *value = format!("{PREFIX}\n\n{value}");
                }
            }
            Value::Array(parts) => {
                if parts.first().is_none_or(|part| part["text"] != PREFIX) {
                    parts.insert(0, json!({"type":"text","text":PREFIX}));
                }
            }
            _ => *content = json!(PREFIX),
        }
    } else {
        messages.insert(0, json!({"role":"system","content":PREFIX}));
    }
    Ok(body)
}
fn headers(
    request: &FetchRequest,
    site: Site,
    access: &str,
    model: &str,
) -> PluginResult<BTreeMap<String, String>> {
    let mut headers = if site == Site::China {
        transport::headers("", now())?
    } else {
        request
            .headers
            .iter()
            .filter(|(key, _)| {
                !matches!(
                    key.to_ascii_lowercase().as_str(),
                    "authorization"
                        | "x-authorization"
                        | "x-harness-type"
                        | "host"
                        | "content-length"
                        | "connection"
                        | "cookie"
                )
            })
            .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
            .collect()
    };
    let version = if site == Site::China {
        transport::CN_VERSION
    } else {
        transport::VERSION
    };
    for (key, value) in [
        ("content-type", "application/json".into()),
        ("accept", "application/json".into()),
        ("user-agent", transport::agent(version)),
        ("x-authorization", format!("Bearer {}", auth::bare(access))),
        ("x-client-type", "pc".into()),
        ("x-product", "autoclaw".into()),
        ("x-tm", transport::platform().into()),
        ("x-version", version.into()),
        ("x-lang", "zh-CN".into()),
        (
            "x-channel",
            if site == Site::China {
                "AutoClaw26"
            } else {
                "AutoClaw4"
            }
            .into(),
        ),
        ("x_trace_id", "autoclaw-desktop".into()),
        ("x-request-model", model.into()),
        ("x-request-id", random_hex(16)?),
    ] {
        headers.insert(key.into(), value);
    }
    if site == Site::China {
        headers.insert("x-agent-id".into(), "main".into());
        headers.insert("x-trace-id".into(), headers["x-request-id"].clone());
    }
    Ok(headers)
}
fn response(
    status: u16,
    mut headers: BTreeMap<String, String>,
    body: Vec<u8>,
    sign_in: &str,
) -> FetchResponse {
    headers.retain(|key, _| {
        !matches!(
            key.to_ascii_lowercase().as_str(),
            "content-length" | "content-encoding"
        )
    });
    headers.insert("content-type".into(), "application/json".into());
    headers.insert("x-magpie-sign-in".into(), sign_in.into());
    FetchResponse {
        status,
        headers,
        body: Box::pin(futures::stream::once(async move { Ok(body) })),
    }
}
fn quota(value: &Value) -> bool {
    if value["code"].as_i64() == Some(810000)
        || value["error"]["code"].as_i64() == Some(810000)
        || value["code"] == "810000"
    {
        return true;
    }
    let message = value
        .get("message")
        .or_else(|| value.get("msg"))
        .or_else(|| value.pointer("/error/message"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    [
        "额度",
        "余额不足",
        "用完",
        "用尽",
        "quota",
        "insufficient_balance",
        "insufficient balance",
        "insufficient_credit",
        "insufficient credit",
        "exhausted",
    ]
    .iter()
    .any(|word| message.contains(word))
}
pub(super) async fn fetch(
    provider: &Autoclaw,
    request: FetchRequest,
) -> PluginResult<FetchResponse> {
    provider.require_subscriptions(request.subscription_accounts_enabled)?;
    if !request.method.eq_ignore_ascii_case("POST") {
        return Err(invalid("AutoClaw accepts chat completion POST requests."));
    }
    let bytes = request
        .decode_body()?
        .ok_or_else(|| invalid("AutoClaw needs a chat request body."))?;
    let body: Value = serde_json::from_slice(&bytes)
        .map_err(|_| invalid("The AutoClaw chat request is not valid JSON."))?;
    let model = text(&body, "model");
    if model.is_empty() || model.len() > 512 || model.chars().any(char::is_control) {
        return Err(invalid("Choose an AutoClaw chat model."));
    }
    let body = prepare(body.clone(), provider.site)?
        .to_string()
        .into_bytes();
    let client = provider.client(request.network_proxy.as_ref())?;
    let credentials = provider
        .current(
            client.as_ref(),
            request.credentials.as_ref().ok_or_else(signed_out)?,
        )
        .await?;
    let headers = headers(&request, provider.site, text(&credentials, "access"), model)?;
    let urls = if provider.site == Site::China {
        vec!["https://autoglm-acceleration-api.zhipuai.cn/autoclaw-proxy/proxy/autoclaw/chat/completions".into()]
    } else {
        [Site::International, Site::China]
            .into_iter()
            .map(|site| {
                format!(
                    "{}/autoclaw-proxy/proxy/autoclaw/v1/chat/completions",
                    site.host()
                )
            })
            .collect()
    };
    let count = urls.len();
    for (index, url) in urls.into_iter().enumerate() {
        let result = transport::send(
            client.as_ref(),
            HttpRequest {
                method: HttpMethod::Post,
                url,
                headers: headers.clone(),
                body: Some(body.clone()),
            },
        )
        .await?;
        if result.status < 400 {
            let mut headers = result.headers;
            headers.insert("x-magpie-sign-in".into(), "kept".into());
            return Ok(FetchResponse {
                status: result.status,
                headers,
                body: Box::pin(futures::StreamExt::map(result.body, |chunk| {
                    chunk.map(|v| v.to_vec()).map_err(|_| {
                        PluginError::transport("The AutoClaw response stream stopped.")
                    })
                })),
            });
        }
        let (status, headers, bytes) = transport::bytes(result, 4 * 1024 * 1024).await?;
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if status == 403 && quota(&value) {
            let mut message = value
                .get("message")
                .or_else(|| value.get("msg"))
                .or_else(|| value.pointer("/error/message"))
                .and_then(Value::as_str)
                .unwrap_or("AutoClaw quota exhausted")
                .to_owned();
            for key in ["access", "refresh"] {
                let secret = auth::bare(text(&credentials, key));
                if !secret.is_empty() {
                    message = message.replace(secret, "[redacted]");
                }
            }
            return Ok(response(
                429,
                headers,
                json!({"error":{"message":message,"type":"rate_limit_error","code":810000}})
                    .to_string()
                    .into_bytes(),
                "kept",
            ));
        }
        if matches!(status, 401 | 403) {
            if index + 1 < count {
                continue;
            }
            return Ok(response(401,headers,json!({"error":{"message":"Sign in to this AutoClaw account again.","type":"invalid_request_error"}}).to_string().into_bytes(),"expired"));
        }
        if status == 406 {
            return Ok(response(
                406,
                headers,
                json!({"error":{"message":HINT,"type":"permission_error"}})
                    .to_string()
                    .into_bytes(),
                "kept",
            ));
        }
        let mut body = String::from_utf8_lossy(&bytes).into_owned();
        for key in ["access", "refresh"] {
            let secret = auth::bare(text(&credentials, key));
            if !secret.is_empty() {
                body = body.replace(secret, "[redacted]");
            }
        }
        return Ok(response(status, headers, body.into_bytes(), "kept"));
    }
    Err(signed_out())
}

#[cfg(test)]
#[path = "../test/chat.test.rs"]
mod tests;
