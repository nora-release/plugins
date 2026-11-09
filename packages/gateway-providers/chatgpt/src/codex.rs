//! ChatGPT's Codex backend as Codex CLI speaks to it: its headers, the
//! Responses bodies it takes, its model list and its usage.

use crate::auth::{signed_out, text};
use nora_plugin_runtime_sdk::provider::{Model, ResetCard, Usage, UsageResets, UsageWindow};
use provider_support::{
    collect_body_text_limited, HttpClient, HttpRequest, HttpStreamingResponse, PluginError,
    PluginResult,
};
use serde_json::{json, Map, Value};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) const BASE: &str = "https://chatgpt.com/backend-api/codex";
pub(crate) const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
/// The rate-limit resets the account holds, each with when it runs out.
pub(crate) const RESET_CREDITS_URL: &str =
    "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits";
/// The Codex CLI the requests say they come from: the backend serves its
/// newest models only to a client that names a recent one.
pub(crate) const VERSION: &str = "0.159.0";

/// What a Responses request may carry that the Codex backend refuses.
const UNSUPPORTED: &[&str] = &[
    "max_output_tokens",
    "max_completion_tokens",
    "temperature",
    "top_p",
    "previous_response_id",
    "user",
    "safety_identifier",
    "prompt_cache_options",
];

fn os_name() -> &'static str {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| {
        let version = std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
            .filter(|version| !version.is_empty());
        match version {
            Some(mut version) => {
                while version.matches('.').count() < 2 {
                    version.push_str(".0");
                }
                format!("Mac OS {version}")
            }
            None => "Mac OS".into(),
        }
    })
}

/// Codex CLI's originator and User-Agent, which every request it makes carries.
pub(crate) fn client_headers(request: HttpRequest) -> HttpRequest {
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x86_64"
    };
    request
        .with_header("originator", "codex_cli_rs")
        .with_header(
            "user-agent",
            format!(
                "codex_cli_rs/{VERSION} ({}; {arch}) Apple_Terminal/455",
                os_name()
            ),
        )
}

/// A call about the account (its usage, its models), signed in.
pub(crate) fn account_request(request: HttpRequest, credentials: &Value) -> HttpRequest {
    client_headers(request)
        .with_header(
            "authorization",
            format!("Bearer {}", text(credentials, "access")),
        )
        .with_header("chatgpt-account-id", text(credentials, "accountID"))
}

/// Sends a request and reads a JSON answer; 401 is a sign-in to renew.
pub(crate) async fn json(
    client: &dyn HttpClient,
    request: HttpRequest,
    timeout: Duration,
) -> PluginResult<Value> {
    let response = send(client, request, timeout).await?;
    let status = response.status;
    let body = collect_body_text_limited(response.body, 8 * 1024 * 1024)
        .await
        .map_err(|_| PluginError::transport("ChatGPT's answer was cut short."))?;
    if status == 401 {
        return Err(signed_out());
    }
    let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    if !(200..300).contains(&status) {
        return Err(PluginError::transport(explain(status, &value)));
    }
    Ok(value)
}

pub(crate) async fn send(
    client: &dyn HttpClient,
    request: HttpRequest,
    timeout: Duration,
) -> PluginResult<HttpStreamingResponse> {
    tokio::time::timeout(timeout, client.stream(request))
        .await
        .map_err(|_| PluginError::transport("ChatGPT took too long to answer."))?
        .map_err(|error| PluginError::transport(format!("ChatGPT could not be reached: {error}")))
}

/// What ChatGPT said went wrong, without the body around it.
pub(crate) fn explain(status: u16, value: &Value) -> String {
    let message = value
        .pointer("/error/message")
        .or_else(|| value.pointer("/detail/message"))
        .or_else(|| value.get("detail"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if message.is_empty() {
        format!("ChatGPT answered HTTP {status}.")
    } else {
        format!("ChatGPT answered HTTP {status}: {message}")
    }
}

/// A Responses request made into one the Codex backend takes: its own
/// instructions for the model, the caller's moved into the input, the
/// fields it refuses taken out, and always streamed, never stored.
pub(crate) fn body(object: &mut Map<String, Value>, instructions: Option<String>) {
    for key in UNSUPPORTED {
        object.remove(*key);
    }
    let mut input = match object.remove("input") {
        Some(Value::String(text)) => vec![json!({
            "type": "message", "role": "user",
            "content": [{"type": "input_text", "text": text}]
        })],
        Some(Value::Array(items)) => items,
        _ => Vec::new(),
    };
    input.retain(|item| item["type"] != "item_reference");
    let own = object
        .remove("instructions")
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default();
    let prompt = match instructions {
        Some(prompt) if !prompt.is_empty() => {
            if !own.trim().is_empty() && first_line(&own) != first_line(&prompt) {
                input.insert(
                    0,
                    json!({"type": "message", "role": "developer", "content": [{"type": "input_text", "text": own}]}),
                );
                prompt
            } else if own.trim().is_empty() {
                prompt
            } else {
                own
            }
        }
        _ => own,
    };
    object.insert("input".into(), Value::Array(input));
    if !prompt.is_empty() {
        object.insert("instructions".into(), Value::String(prompt));
    }
    object.insert("store".into(), json!(false));
    object.insert("stream".into(), json!(true));
    object.entry("tool_choice").or_insert(json!("auto"));
    object.entry("parallel_tool_calls").or_insert(json!(true));
    object
        .entry("text")
        .or_insert(json!({"verbosity": "medium"}));
    let effort = object
        .get_mut("reasoning")
        .and_then(Value::as_object_mut)
        .map(|reasoning| {
            if reasoning.get("effort").and_then(Value::as_str) == Some("ultra") {
                reasoning.insert("effort".into(), json!("max"));
            }
            let effort = reasoning
                .get("effort")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            if effort != "none" {
                reasoning.entry("summary").or_insert(json!("auto"));
            }
            effort
        })
        .unwrap_or_default();
    if effort == "none" {
        object.remove("include");
        return;
    }
    let mut include = object
        .get("include")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !include.contains(&json!("reasoning.encrypted_content")) {
        include.push(json!("reasoning.encrypted_content"));
    }
    object.insert("include".into(), Value::Array(include));
}

fn first_line(text: &str) -> &str {
    let text = text.trim();
    text.split_once('\n').map_or(text, |(first, _)| first)
}

/// A model of the list and the instructions the backend wants with it.
pub(crate) struct Listed {
    pub model: Model,
    pub instructions: Option<String>,
}

/// The models `/codex/models` lists for the account, in its order, the
/// hidden left out.
pub(crate) fn models(value: &Value) -> PluginResult<Vec<Listed>> {
    let mut listed: Vec<&Value> = value["models"]
        .as_array()
        .ok_or_else(|| PluginError::transport("ChatGPT listed no models."))?
        .iter()
        .filter(|model| {
            model["slug"].as_str().is_some_and(|slug| !slug.is_empty())
                && model["visibility"] != "hide"
        })
        .collect();
    listed.sort_by_key(|model| model["priority"].as_i64().unwrap_or(i64::MAX));
    Ok(listed
        .into_iter()
        .map(|model| {
            let id = model["slug"].as_str().unwrap_or_default();
            let instructions = model
                .pointer("/model_messages/instructions_template")
                .or_else(|| model.get("base_instructions"))
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_owned);
            Listed {
                model: Model {
                    name: model["display_name"]
                        .as_str()
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned),
                    context: model["context_window"]
                        .as_u64()
                        .filter(|window| *window > 0),
                    reasoning: model["supported_reasoning_levels"]
                        .as_array()
                        .is_some_and(|levels| !levels.is_empty()),
                    images: model["input_modalities"]
                        .as_array()
                        .is_some_and(|inputs| inputs.iter().any(|input| input == "image")),
                    ..Model::new(id)
                },
                instructions,
            }
        })
        .collect())
}

/// `/wham/usage` as the router reads usage: the plan, its two windows, the
/// credits left and how many rate-limit resets it holds.
pub(crate) fn usage(value: &Value) -> Usage {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut windows = Vec::new();
    for key in ["primary_window", "secondary_window"] {
        let window = &value["rate_limit"][key];
        if !window.is_object() {
            continue;
        }
        let seconds = window["limit_window_seconds"].as_u64().unwrap_or(0);
        let resets = window["reset_at"]
            .as_u64()
            .filter(|at| *at > 0)
            .or_else(|| {
                window["reset_after_seconds"]
                    .as_u64()
                    .map(|after| now + after)
            });
        windows.push(UsageWindow {
            name: window_name(seconds),
            used: window["used_percent"].as_f64(),
            resets_at: resets.map(|at| {
                nora_plugin_runtime_sdk::provider::rfc3339(UNIX_EPOCH + Duration::from_secs(at))
            }),
            ..UsageWindow::default()
        });
    }
    if let Some(balance) = value
        .pointer("/credits/balance")
        .and_then(|balance| {
            balance
                .as_f64()
                .or_else(|| balance.as_str().and_then(|text| text.parse().ok()))
        })
        .filter(|_| value.pointer("/credits/has_credits") != Some(&json!(false)))
    {
        windows.push(UsageWindow {
            name: "Credits".into(),
            balance: Some(balance),
            unit: Some("credits".into()),
            ..UsageWindow::default()
        });
    }
    let resets = value
        .pointer("/rate_limit_reset_credits/available_count")
        .and_then(Value::as_u64)
        .filter(|count| *count > 0)
        .map(|count| UsageResets {
            count: u32::try_from(count).unwrap_or(u32::MAX),
            cards: Vec::new(),
        });
    Usage {
        plan: value["plan_type"]
            .as_str()
            .filter(|plan| !plan.is_empty())
            .map(str::to_owned),
        windows,
        resets,
    }
}

/// The resets `/wham/rate-limit-reset-credits` lists still to be used,
/// the soonest to run out first, one that never does last.
pub(crate) fn reset_cards(value: &Value) -> Vec<ResetCard> {
    let mut cards: Vec<ResetCard> = value["credits"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|credit| credit["status"] == "available")
        .filter_map(|credit| {
            let id = credit["id"].as_str().filter(|id| !id.is_empty())?;
            Some(ResetCard {
                id: Some(id.to_owned()),
                expires_at: credit["expires_at"]
                    .as_str()
                    .filter(|at| !at.is_empty())
                    .map(str::to_owned),
            })
        })
        .collect();
    // RFC 3339 times in one zone sort as text
    cards.sort_by(|a, b| match (&a.expires_at, &b.expires_at) {
        (Some(a), Some(b)) => a.cmp(b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    cards
}

fn window_name(seconds: u64) -> String {
    match seconds {
        0 => "Usage".into(),
        s if s % 86_400 == 0 => format!("{} days", s / 86_400),
        s if s % 3_600 == 0 => format!("{} hours", s / 3_600),
        s => format!("{} minutes", s / 60),
    }
}

/// The response a stream ends on (`response.completed`), for a caller that
/// asked for one answer rather than a stream.
pub(crate) fn completed(stream: &str) -> Option<Value> {
    stream
        .split("\n\n")
        .filter_map(|event| {
            let data: String = event
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim_start)
                .collect::<Vec<_>>()
                .join("\n");
            serde_json::from_str::<Value>(&data).ok()
        })
        .filter(|event| {
            matches!(
                event["type"].as_str(),
                Some("response.completed" | "response.incomplete" | "response.failed")
            )
        })
        .last()
        .map(|event| event["response"].clone())
}

#[cfg(test)]
#[path = "../test/codex.test.rs"]
mod tests;
