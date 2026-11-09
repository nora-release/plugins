//! Signing in to xAI as Grok Build does: the device flow at auth.x.ai, the
//! account read from the ID token, and the access token renewed with the
//! refresh token, which xAI turns over at each renewal.

use base64::Engine;
use nora_plugin_runtime_sdk::provider::{rfc3339, Account};
use provider_support::{
    collect_body_text_limited, explain, DeviceFlow, HttpClient, HttpMethod, HttpRequest,
    PluginError, PluginResult,
};
use serde_json::{json, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
pub(crate) const DEVICE_CODE_URL: &str = "https://auth.x.ai/oauth2/device/code";
pub(crate) const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const SCOPE: &str = "openid profile email offline_access grok-cli:access api:access conversations:read conversations:write workspaces:read workspaces:write";

pub(crate) fn signed_out() -> PluginError {
    PluginError::authentication("Sign in to Grok again.")
}

/// xAI's device sign-in, as the Grok Build version named.
pub(crate) fn device_flow(version: &str) -> DeviceFlow {
    DeviceFlow {
        vendor: "xAI",
        device_code_url: DEVICE_CODE_URL,
        token_url: TOKEN_URL,
        client_id: CLIENT_ID,
        scope: SCOPE,
        verification_url: "https://accounts.x.ai/oauth2/device",
        headers: vec![("x-grok-client-version", version.to_owned())],
    }
}

/// Renews the access token with the refresh token.
pub(crate) async fn refresh(
    client: &dyn HttpClient,
    saved: &Value,
    version: &str,
) -> PluginResult<Value> {
    let refresh = text(saved, "refresh");
    if refresh.is_empty() {
        return Err(signed_out());
    }
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", CLIENT_ID)
        .append_pair("grant_type", "refresh_token")
        .append_pair("refresh_token", refresh)
        .finish();
    let request = HttpRequest::new(HttpMethod::Post, TOKEN_URL)
        .with_header("accept", "application/json")
        .with_header("content-type", "application/x-www-form-urlencoded")
        .with_header("x-grok-client-version", version)
        .with_body(form.into_bytes());
    let response = tokio::time::timeout(Duration::from_secs(30), client.stream(request))
        .await
        .map_err(|_| PluginError::transport("xAI took too long to renew the sign-in."))?
        .map_err(|error| PluginError::transport(format!("xAI could not be reached: {error}")))?;
    let status = response.status;
    let body = collect_body_text_limited(response.body, 1024 * 1024)
        .await
        .map_err(|_| PluginError::transport("xAI's answer was cut short."))?;
    let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    // a refresh token spent or revoked: only a new sign-in helps
    if matches!(status, 400 | 401 | 403) {
        return Err(PluginError::authentication(explain(
            "xAI would not renew the Grok sign-in; sign in again",
            status,
            &value,
        )));
    }
    if !(200..300).contains(&status) {
        return Err(PluginError::transport(explain(
            "xAI did not renew the Grok sign-in",
            status,
            &value,
        )));
    }
    credentials(saved, &value)
}

/// The credentials kept for an account, from what the token endpoint
/// answered and what was kept before (a renewal may leave fields out).
pub(crate) fn credentials(previous: &Value, tokens: &Value) -> PluginResult<Value> {
    let access = text(tokens, "access_token");
    if access.is_empty() {
        return Err(signed_out());
    }
    let mut next = if previous.is_object() {
        previous.clone()
    } else {
        json!({})
    };
    next["access"] = json!(access);
    for (key, field) in [("refresh", "refresh_token"), ("idToken", "id_token")] {
        let value = text(tokens, field);
        if !value.is_empty() {
            next[key] = json!(value);
        }
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let expires = tokens["expires_in"]
        .as_u64()
        .map(|seconds| now + seconds)
        .or_else(|| claims(access)["exp"].as_u64());
    if let Some(expires) = expires {
        next["expires"] = json!(expires);
    }
    let id = claims(text(&next, "idToken"));
    for (key, claim) in [("email", "email"), ("userID", "sub"), ("name", "name")] {
        if let Some(value) = id[claim].as_str().filter(|value| !value.is_empty()) {
            next[key] = json!(value);
        }
    }
    Ok(next)
}

/// The account as the router keeps it: named by its email, else its xAI
/// user id (an account signed in through X can have no email).
pub(crate) fn account(credentials: Value) -> Account {
    let name = ["email", "userID", "name"]
        .iter()
        .map(|key| text(&credentials, key))
        .find(|name| !name.is_empty())
        .unwrap_or("Grok")
        .to_owned();
    Account {
        name,
        expires_at: expires_at(&credentials),
        credentials,
    }
}

pub(crate) fn expires_at(credentials: &Value) -> Option<String> {
    credentials["expires"]
        .as_u64()
        .map(|seconds| rfc3339(UNIX_EPOCH + Duration::from_secs(seconds)))
}

/// Whether the access token ends within `margin`.
pub(crate) fn ending(credentials: &Value, margin: Duration) -> bool {
    let Some(expires) = credentials["expires"].as_u64() else {
        return false;
    };
    SystemTime::now() + margin >= UNIX_EPOCH + Duration::from_secs(expires)
}

/// A JWT's claims, unchecked: the token came from the token endpoint.
pub(crate) fn claims(token: &str) -> Value {
    token
        .split('.')
        .nth(1)
        .and_then(|part| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(part.trim_end_matches('='))
                .ok()
        })
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null)
}

pub(crate) fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

#[cfg(test)]
#[path = "../test/xai.test.rs"]
pub(crate) mod tests;
