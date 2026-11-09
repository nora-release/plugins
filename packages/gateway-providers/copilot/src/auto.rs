//! Copilot's Auto: the model Copilot picks for the account. A Free plan may
//! pick little by hand (gpt-4.1); its other models are served only as
//! Auto's pick, with the session token Auto gives sent beside each request.
//!
//! VS Code's Copilot Chat asks POST {api}/models/session with
//! `{"auto_mode":{"model_hints":["auto"]}}` (API version 2025-10-01), or,
//! from 0.69, POST {api}/auto with the turn's prompt (2026-08-01): both
//! answer a `session_token`, the `selected_model` and when it expires.

use crate::copilot::{api_headers, Session};
use crate::github;
use provider_support::{HttpClient, HttpMethod, HttpRequest, PluginError, PluginResult};
use serde_json::{json, Value};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The model id that has Copilot pick the model.
pub(crate) const AUTO: &str = "auto";
/// How long a pick is used for: Copilot is asked again within the hour.
const REUSE: Duration = Duration::from_secs(3600);
/// A pick is asked again this long before its session expires.
const EARLY: Duration = Duration::from_secs(120);

/// Auto's pick: the model the requests name and the session they carry.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Pick {
    pub token: String,
    pub model: String,
    pub expires: SystemTime,
    pub picked: Instant,
}

impl Pick {
    pub(crate) fn usable(&self) -> bool {
        self.picked.elapsed() < REUSE && SystemTime::now() + EARLY < self.expires
    }
}

/// An Auto session's answer read: the model is `selected_model`'s id (a
/// string, or the model itself), else the first of `available_models`.
pub(crate) fn pick(value: &Value) -> Option<Pick> {
    let token = value["session_token"]
        .as_str()
        .filter(|token| !token.is_empty())?;
    let selected = &value["selected_model"];
    let model = selected
        .as_str()
        .or_else(|| selected["id"].as_str())
        .or_else(|| value["available_models"][0].as_str())
        .filter(|model| !model.is_empty())?;
    let expires = value["expires_at"].as_u64().map_or_else(
        || SystemTime::now() + REUSE,
        |at| UNIX_EPOCH + Duration::from_secs(at),
    );
    Some(Pick {
        token: token.to_owned(),
        model: model.to_owned(),
        expires,
        picked: Instant::now(),
    })
}

/// Asks Copilot for Auto's pick: /models/session, then /auto.
pub(crate) async fn ask(client: &dyn HttpClient, session: &Session) -> PluginResult<Pick> {
    let asks = [
        (
            "/models/session",
            "2025-10-01",
            json!({"auto_mode": {"model_hints": ["auto"]}}),
        ),
        ("/auto", "2026-08-01", json!({"prompt": "Hello"})),
    ];
    let mut why = String::new();
    for (path, version, body) in asks {
        let request = api_headers(
            HttpRequest::new(HttpMethod::Post, format!("{}{path}", session.api)),
            session,
        )
        .with_header("x-github-api-version", version)
        .with_json_body(&body)
        .map_err(|_| PluginError::invalid("Auto's question could not be encoded."))?;
        let (status, value) = github::json(client, request, Duration::from_secs(30)).await?;
        if let Some(pick) = pick(&value).filter(|_| (200..300).contains(&status)) {
            return Ok(pick);
        }
        why = github::explain("Copilot's Auto picked no model", status, &value);
    }
    Err(PluginError::transport(why))
}

/// Whether Copilot turned a request of Auto's pick away for its session:
/// the session gone, or the pick no longer the session's.
pub(crate) fn session_refused(status: u16, value: &Value) -> bool {
    let message = value["error"]["message"]
        .as_str()
        .or_else(|| value["message"].as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(status, 400 | 401 | 403 | 404) && message.contains("session")
}

#[cfg(test)]
#[path = "../test/auto.test.rs"]
mod tests;
