//! Signing in to ChatGPT as Codex CLI does: OAuth with PKCE at
//! auth.openai.com, the browser coming back to port 1455 on this machine,
//! and the tokens renewed with the refresh token.

use crate::codex;
use base64::Engine;
use nora_plugin_runtime_sdk::provider::Account;
use provider_support::{
    random_hex, HttpClient, HttpMethod, HttpRequest, OAuthCallbackServer,
    OAuthCallbackServerConfig, PluginError, PluginResult,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

pub(crate) const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub(crate) const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
pub(crate) const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const SCOPES: &str = "openid profile email offline_access";
const CALLBACK_PORT: u16 = 1455;
const CALLBACK_PATH: &str = "/auth/callback";

/// A browser sign-in waiting on its user: the callback server's wait, then
/// the code's exchange, in a task of its own.
pub(crate) struct Pending {
    task: Option<tokio::task::JoinHandle<PluginResult<Value>>>,
}

impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl Pending {
    /// The credentials once the browser came back and the code was
    /// exchanged, none while the user is still signing in.
    pub(crate) async fn poll(&mut self) -> Option<PluginResult<Value>> {
        if !self.task.as_ref().is_some_and(|task| task.is_finished()) {
            return self.task.is_none().then(|| Err(signed_out()));
        }
        let task = self.task.take()?;
        Some(task.await.unwrap_or_else(|_| Err(signed_out())))
    }
}

pub(crate) fn signed_out() -> PluginError {
    PluginError::authentication("Sign in with ChatGPT again.")
}

fn challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Opens the callback port and starts waiting for the browser: the page to
/// open, and the sign-in that follows it.
pub(crate) async fn start(client: Arc<dyn HttpClient>) -> PluginResult<(String, Pending)> {
    let verifier = random_hex(48);
    let state = random_hex(24);
    let config = OAuthCallbackServerConfig::with_path(CALLBACK_PORT, CALLBACK_PATH)
        .with_redirect_host("localhost");
    let mut server = OAuthCallbackServer::with_config(config);
    server.expect_state(state.clone());
    server.start().await.map_err(|error| {
        PluginError::transport(format!(
            "ChatGPT's sign-in needs port {CALLBACK_PORT} on this Mac: {error}"
        ))
    })?;
    let redirect = server.redirect_uri();
    let mut url = url::Url::parse(AUTHORIZE_URL).expect("a valid authorize URL");
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", &redirect)
        .append_pair("scope", SCOPES)
        .append_pair("code_challenge", &challenge(&verifier))
        .append_pair("code_challenge_method", "S256")
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("state", &state)
        .append_pair("originator", "codex_cli_rs");
    let task = tokio::spawn(async move {
        let params = tokio::time::timeout(Duration::from_secs(600), server.wait_for_callback())
            .await
            .map_err(|_| PluginError::authentication("The ChatGPT sign-in timed out."))?
            .map_err(|error| PluginError::authentication(error.to_string()))?;
        let code = params
            .get("code")
            .filter(|code| !code.is_empty())
            .ok_or_else(|| PluginError::authentication("ChatGPT sent no sign-in code."))?;
        exchange(client.as_ref(), code, &redirect, &verifier).await
    });
    Ok((url.to_string(), Pending { task: Some(task) }))
}

async fn exchange(
    client: &dyn HttpClient,
    code: &str,
    redirect: &str,
    verifier: &str,
) -> PluginResult<Value> {
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("code", code)
        .append_pair("redirect_uri", redirect)
        .append_pair("client_id", CLIENT_ID)
        .append_pair("code_verifier", verifier)
        .finish();
    let request = codex::client_headers(HttpRequest::new(HttpMethod::Post, TOKEN_URL))
        .with_header("content-type", "application/x-www-form-urlencoded")
        .with_body(form.into_bytes());
    let tokens = codex::json(client, request, Duration::from_secs(30)).await?;
    credentials(&Value::Null, &tokens)
}

/// Renews the access token with the refresh token, as Codex CLI does.
pub(crate) async fn refresh(client: &dyn HttpClient, saved: &Value) -> PluginResult<Value> {
    let refresh = text(saved, "refresh");
    if refresh.is_empty() {
        return Err(signed_out());
    }
    let body = json!({"client_id": CLIENT_ID, "grant_type": "refresh_token", "refresh_token": refresh, "refresh_reason": "expired"});
    let request = codex::client_headers(HttpRequest::new(HttpMethod::Post, TOKEN_URL))
        .with_json_body(&body)
        .map_err(|_| PluginError::invalid("The refresh could not be encoded."))?;
    let tokens = codex::json(client, request, Duration::from_secs(30)).await?;
    credentials(saved, &tokens)
}

/// The credentials kept for an account, from what the token endpoint
/// answered and what was kept before (a refresh may leave fields out).
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
    let id = claims(text(&next, "idToken"));
    let auth = &id["https://api.openai.com/auth"];
    for (key, value) in [
        ("email", id["email"].as_str()),
        ("accountID", auth["chatgpt_account_id"].as_str()),
        ("plan", auth["chatgpt_plan_type"].as_str()),
    ] {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            next[key] = json!(value);
        }
    }
    if let Some(expires) = claims(access)["exp"].as_u64() {
        next["expires"] = json!(expires);
    }
    if text(&next, "accountID").is_empty() {
        return Err(PluginError::authentication(
            "ChatGPT named no account for this sign-in.",
        ));
    }
    Ok(next)
}

/// The account as the router keeps it: named by its email.
pub(crate) fn account(credentials: Value) -> Account {
    let name = [text(&credentials, "email"), text(&credentials, "accountID")]
        .into_iter()
        .find(|name| !name.is_empty())
        .unwrap_or("ChatGPT")
        .to_owned();
    Account {
        name,
        expires_at: expires_at(&credentials),
        credentials,
    }
}

pub(crate) fn expires_at(credentials: &Value) -> Option<String> {
    credentials["expires"].as_u64().map(|seconds| {
        nora_plugin_runtime_sdk::provider::rfc3339(UNIX_EPOCH + Duration::from_secs(seconds))
    })
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
#[path = "../test/auth.test.rs"]
pub(crate) mod tests;
