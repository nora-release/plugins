//! Copilot's API as VS Code's Copilot Chat speaks to it: the session token
//! a GitHub token is traded for, the headers each request carries, the
//! models an account may pick and the allowance it has used.

use nora_plugin_runtime_sdk::provider::{Model, Usage, UsageWindow};
use provider_support::HttpRequest;
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) const TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";
pub(crate) const USER_URL: &str = "https://api.github.com/copilot_internal/user";
/// Where requests go when the session names no address of its own.
pub(crate) const API: &str = "https://api.githubcopilot.com";

/// A session token is renewed this long before it expires.
const EARLY: Duration = Duration::from_secs(120);

/// What VS Code's Copilot Chat says it is on every call to GitHub.
pub(crate) fn editor_headers(request: HttpRequest) -> HttpRequest {
    request
        .with_header("editor-version", "vscode/1.140.0")
        .with_header("editor-plugin-version", "copilot-chat/0.68.0")
        .with_header("copilot-integration-id", "vscode-chat")
        .with_header("user-agent", "GitHubCopilotChat/0.68.0")
}

/// The Copilot API's own headers, signed in with the session token.
pub(crate) fn api_headers(request: HttpRequest, session: &Session) -> HttpRequest {
    editor_headers(request)
        .with_header("x-github-api-version", "2026-01-09")
        .with_header("authorization", format!("Bearer {}", session.token))
}

/// A short-lived token for Copilot's API and the address it serves.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Session {
    pub token: String,
    pub api: String,
    pub expires: SystemTime,
}

impl Session {
    pub(crate) fn fresh(&self) -> bool {
        SystemTime::now() + EARLY < self.expires
    }
}

/// The session token's answer read, or none when it holds no token.
pub(crate) fn session(value: &Value) -> Option<Session> {
    let token = value["token"].as_str().filter(|token| !token.is_empty())?;
    let api = value
        .pointer("/endpoints/api")
        .and_then(Value::as_str)
        .filter(|api| api.starts_with("https://"))
        .unwrap_or(API)
        .trim_end_matches('/');
    let expires = value["expires_at"].as_u64().map_or_else(
        || SystemTime::now() + Duration::from_secs(600),
        |at| UNIX_EPOCH + Duration::from_secs(at),
    );
    Some(Session {
        token: token.to_owned(),
        api: api.to_owned(),
        expires,
    })
}

/// The chat models `/models` offers the account on Chat Completions, as
/// Copilot's model picker offers them.
pub(crate) fn models(value: &Value) -> Vec<Model> {
    let Some(list) = value["data"].as_array() else {
        return Vec::new();
    };
    list.iter()
        .filter(|model| offered(model))
        .map(|model| {
            let limits = &model["capabilities"]["limits"];
            let supports = &model["capabilities"]["supports"];
            Model {
                name: model["name"]
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned),
                context: limits["max_context_window_tokens"]
                    .as_u64()
                    .or_else(|| limits["max_prompt_tokens"].as_u64())
                    .filter(|tokens| *tokens > 0),
                output: limits["max_output_tokens"]
                    .as_u64()
                    .filter(|tokens| *tokens > 0),
                reasoning: supports["reasoning_effort"]
                    .as_array()
                    .is_some_and(|efforts| !efforts.is_empty())
                    || supports["adaptive_thinking"] == true
                    || supports["max_thinking_budget"].as_u64().is_some(),
                images: supports["vision"] == true,
                ..Model::new(model["id"].as_str().unwrap_or_default())
            }
        })
        .collect()
}

/// Whether a `/models` entry is a chat model this account may pick and
/// Chat Completions serves: not Copilot's own helpers, and enabled for it.
fn offered(model: &Value) -> bool {
    let id = model["id"].as_str().unwrap_or_default();
    if id.is_empty() || model["capabilities"]["type"] != "chat" || internal(id) {
        return false;
    }
    let picker = model["model_picker_enabled"] == true;
    if !picker {
        return false;
    }
    let enabled = match model["policy"]["state"].as_str() {
        Some(state) => state == "enabled",
        None => true,
    };
    let endpoints = model["supported_endpoints"].as_array();
    let chat = endpoints
        .is_none_or(|list| list.is_empty() || list.iter().any(|e| e == "/chat/completions"));
    enabled && chat
}

/// Copilot's own models, which no client offers to pick.
fn internal(id: &str) -> bool {
    ["copilot-search", "exec-agent", "trajectory"]
        .iter()
        .any(|prefix| id.starts_with(prefix))
        || ["-secondary", "-tertiary", "-4th", "-free-auto"]
            .iter()
            .any(|suffix| id.ends_with(suffix))
}

/// Who started a Chat Completions turn: the user typing, or the agent
/// answering a tool. Copilot bills a premium request for the user's only.
pub(crate) fn initiator(body: &Value) -> &'static str {
    let last = body["messages"]
        .as_array()
        .and_then(|messages| messages.last());
    match last {
        Some(message) if message["role"] == "user" => "user",
        _ => "agent",
    }
}

/// Whether a Chat Completions body carries an image.
pub(crate) fn has_image(body: &Value) -> bool {
    body["messages"].as_array().is_some_and(|messages| {
        messages.iter().any(|message| {
            message["content"]
                .as_array()
                .is_some_and(|parts| parts.iter().any(|part| part["type"] == "image_url"))
        })
    })
}

/// `/copilot_internal/user` as the router reads usage: the plan and each
/// allowance that has a limit.
pub(crate) fn usage(value: &Value) -> Usage {
    let resets = value["quota_reset_date_utc"]
        .as_str()
        .filter(|at| !at.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            value["quota_reset_date"]
                .as_str()
                .filter(|day| day.len() == 10)
                .map(|day| format!("{day}T00:00:00Z"))
        });
    let mut windows = Vec::new();
    for (key, name) in [
        ("premium_interactions", "Premium requests"),
        ("chat", "Chat requests"),
        ("completions", "Completions"),
    ] {
        let snapshot = &value["quota_snapshots"][key];
        if !snapshot.is_object() || snapshot["unlimited"] == true {
            continue;
        }
        let entitlement = number(&snapshot["entitlement"]);
        let remaining =
            number(&snapshot["quota_remaining"]).or_else(|| number(&snapshot["remaining"]));
        let used = number(&snapshot["percent_remaining"])
            .map(|left| 100.0 - left)
            .or_else(|| match (entitlement, remaining) {
                (Some(all), Some(left)) if all > 0.0 => Some((all - left) / all * 100.0),
                _ => None,
            })
            .map(|used| used.clamp(0.0, 100.0));
        windows.push(UsageWindow {
            name: name.into(),
            used,
            resets_at: resets.clone(),
            balance: remaining.map(|left| left.max(0.0)),
            unit: remaining.map(|_| "requests".into()),
        });
    }
    Usage {
        plan: value["copilot_plan"]
            .as_str()
            .filter(|plan| !plan.is_empty())
            .map(str::to_owned),
        windows,
    }
}

/// A number, or a string of one, as Copilot sends its quotas.
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
        .filter(|number: &f64| number.is_finite())
}

#[cfg(test)]
#[path = "../test/copilot.test.rs"]
mod tests;
