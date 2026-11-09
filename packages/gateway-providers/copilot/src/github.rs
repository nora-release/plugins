//! Signing in to GitHub as Copilot's editors do: the device flow of their
//! OAuth app, whose code the user types at github.com/login/device, and
//! the GitHub account it signed in to.

use nora_plugin_runtime_sdk::provider::Account;
use provider_support::{
    collect_body_text_limited, DeviceFlow, HttpClient, HttpMethod, HttpRequest, PluginError,
    PluginResult,
};
use serde_json::{json, Value};
use std::time::Duration;

pub(crate) const CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
pub(crate) const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
pub(crate) const ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
pub(crate) const USER_URL: &str = "https://api.github.com/user";

pub(crate) fn signed_out() -> PluginError {
    PluginError::authentication("Sign in to GitHub Copilot again.")
}

/// GitHub's device sign-in, with the editors' OAuth app.
pub(crate) fn device_flow() -> DeviceFlow {
    DeviceFlow {
        vendor: "GitHub",
        device_code_url: DEVICE_CODE_URL,
        token_url: ACCESS_TOKEN_URL,
        client_id: CLIENT_ID,
        scope: "read:user",
        verification_url: "https://github.com/login/device",
        headers: crate::copilot::EDITOR
            .iter()
            .map(|(name, value)| (*name, (*value).to_owned()))
            .collect(),
    }
}

/// Sends a request and reads its JSON answer, whatever its status.
pub(crate) async fn json(
    client: &dyn HttpClient,
    request: HttpRequest,
    timeout: Duration,
) -> PluginResult<(u16, Value)> {
    let response = tokio::time::timeout(timeout, client.stream(request))
        .await
        .map_err(|_| PluginError::transport("GitHub took too long to answer."))?
        .map_err(|error| PluginError::transport(format!("GitHub could not be reached: {error}")))?;
    let status = response.status;
    let body = collect_body_text_limited(response.body, 8 * 1024 * 1024)
        .await
        .map_err(|_| PluginError::transport("GitHub's answer was cut short."))?;
    Ok((status, serde_json::from_str(&body).unwrap_or(Value::Null)))
}

/// The account the token signed in to, named by its login.
pub(crate) async fn account(client: &dyn HttpClient, token: &str) -> PluginResult<Account> {
    let request = crate::copilot::editor_headers(HttpRequest::new(HttpMethod::Get, USER_URL))
        .with_header("authorization", format!("token {token}"))
        .with_header("accept", "application/json");
    let (status, value) = json(client, request, Duration::from_secs(30)).await?;
    if status == 401 {
        return Err(signed_out());
    }
    let login = value["login"].as_str().unwrap_or_default();
    if !(200..300).contains(&status) || login.is_empty() {
        return Err(PluginError::transport(explain(
            "GitHub did not say which account signed in",
            status,
            &value,
        )));
    }
    Ok(Account {
        name: login.to_owned(),
        credentials: json!({"github": token, "login": login}),
        expires_at: None,
    })
}

pub(crate) use provider_support::explain;

#[cfg(test)]
#[path = "../test/github.test.rs"]
mod tests;
