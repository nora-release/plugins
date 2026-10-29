use super::{available_count, parse};
use chrono::{DateTime, Utc};
use nora_ai::{HttpClient, HttpMethod, HttpRequest, ResolvedProviderAuth};
use nora_provider::usage::{header, request_json};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::time::Duration;

const ENDPOINT: &str = "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits";

fn failed() -> PluginError {
    PluginError::transport("The reset could not be verified. Refresh this account's usage before trying again; a reset may already have been used.")
}

fn first(payload: &Value, now: i64) -> Option<String> {
    if available_count(payload) == Some(0) {
        return None;
    }
    super::prepare::earliest(&parse(payload), now).map(|(id, _)| id)
}

#[cfg(test)]
pub async fn consume(
    auth: &ResolvedProviderAuth,
    request_id: &Value,
    client: &dyn HttpClient,
) -> PluginResult<Value> {
    consume_options(auth, request_id, None, client).await
}

pub async fn consume_checked(
    auth: &ResolvedProviderAuth,
    params: &Value,
    client: &dyn HttpClient,
) -> PluginResult<Value> {
    if params["requestID"]
        .as_str()
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .is_none_or(|id| id.is_nil())
    {
        return Err(PluginError::invalid("A reset request ID is required."));
    }
    let reason = params["reason"].as_str().unwrap_or("hand");
    if !matches!(reason, "hand" | "week" | "expiring") {
        return Err(PluginError::invalid("The reset reason is invalid."));
    }
    if reason != "hand" && params["offerID"].as_str().is_none_or(str::is_empty) {
        return Err(PluginError::invalid("A reset offer is required."));
    }
    if reason != "hand" {
        let reading = super::prepare::prepare(auth, client).await?;
        let now = Utc::now();
        let eligible = if reason == "week" {
            reading["weekly"]["exhausted"] == true
                && reading["weekly"]["cycleEndsAt"]
                    .as_str()
                    .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
                    .is_some_and(|v| v > now)
        } else {
            reading["windowsUsed"] == true
                && reading["expiring"]["dueAt"]
                    .as_str()
                    .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
                    .is_some_and(|v| v <= now)
                && reading["expiring"]["creditExpiresAt"]
                    .as_str()
                    .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
                    .is_some_and(|v| v > now)
        };
        if !eligible || reading["offerID"] != params["offerID"] {
            return Ok(json!({"outcome":"nothing_to_reset","retrySafe":true}));
        }
    }
    consume_options(
        auth,
        &params["requestID"],
        params["offerID"].as_str(),
        client,
    )
    .await
}

async fn consume_options(
    auth: &ResolvedProviderAuth,
    request_id: &Value,
    offer: Option<&str>,
    client: &dyn HttpClient,
) -> PluginResult<Value> {
    let request_id = request_id
        .as_str()
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .filter(|id| !id.is_nil())
        .ok_or_else(|| PluginError::invalid("A reset request ID is required."))?
        .hyphenated()
        .to_string();
    if auth.credential_provider_id.as_deref() != Some("openai-codex") {
        return Err(PluginError::unsupported("reset credits for this account"));
    }
    let token = auth
        .token
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| PluginError::authentication("Connect a ChatGPT account."))?;
    let account = header(auth, "chatgpt-account-id")
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 512
                && id.trim() == *id
                && !id.chars().any(char::is_control)
        })
        .ok_or_else(|| {
            PluginError::authentication("This ChatGPT account has no account ID. Sign in again.")
        })?;
    let request = HttpRequest::new(HttpMethod::Get, ENDPOINT)
        .with_header("authorization", format!("Bearer {token}"))
        .with_header("chatgpt-account-id", account)
        .with_header("accept", "application/json")
        .with_header("user-agent", "codex-cli")
        .with_header("origin", "https://chatgpt.com")
        .with_header("referer", "https://chatgpt.com/");
    let payload = request_json(client, request.clone(), Duration::from_secs(15))
        .await
        .map_err(|_| {
            PluginError::transport("Reset credits could not be read; no reset was used.")
        })?;
    if payload["error"].is_object() || parse(&payload)["available"] != true {
        return Err(PluginError::transport(
            "Reset credits could not be verified; no reset was used.",
        ));
    }
    let Some(credit) = first(&payload, Utc::now().timestamp_millis()) else {
        return Ok(
            json!({"ok":true,"outcome":"no_credit","message":"No reset credit is available for this account."}),
        );
    };
    if offer.is_some_and(|offer| offer != credit) {
        return Ok(json!({"outcome":"nothing_to_reset","retrySafe":true}));
    }
    let mut request = request;
    request.method = HttpMethod::Post;
    request.url = format!("{ENDPOINT}/consume");
    request
        .headers
        .insert("content-type".into(), "application/json".into());
    request.body = Some(
        serde_json::to_vec(&json!({"credit_id":credit,"redeem_request_id":request_id}))
            .map_err(|_| failed())?,
    );
    // Never retry an ambiguous POST: a lost reply can follow a successful spend.
    let reply = request_json(client, request, Duration::from_secs(20))
        .await
        .map_err(|_| failed())?;
    let (outcome, message) = match reply["code"].as_str() {
        Some("reset") => {
            let windows = reply["windows_reset"]
                .as_u64()
                .filter(|windows| *windows > 0 && *windows <= 100)
                .ok_or_else(failed)?;
            return Ok(
                json!({"ok":true,"outcome":"reset","windowsReset":windows,"message":"The reset credit was used. Usage windows have started again."}),
            );
        }
        Some("nothing_to_reset") => (
            "nothing_to_reset",
            "No window needed a reset. The reset credit was kept.",
        ),
        Some("no_credit") => (
            "no_credit",
            "No reset credit is available for this account.",
        ),
        Some("already_redeemed") => (
            "already_redeemed",
            "This reset credit has already been used.",
        ),
        _ => return Err(failed()),
    };
    Ok(json!({"ok":true,"outcome":outcome,"message":message}))
}

#[cfg(test)]
#[path = "../../../test/reset_consume.test.rs"]
mod tests;
