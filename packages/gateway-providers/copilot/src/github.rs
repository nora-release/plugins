//! Signing in to GitHub as Copilot's editors do: the device flow of their
//! OAuth app, whose code the user types at github.com/login/device, and
//! the GitHub account it signed in to.

use nora_plugin_runtime_sdk::provider::Account;
use provider_support::{
    collect_body_text_limited, HttpClient, HttpMethod, HttpRequest, PluginError, PluginResult,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub(crate) const CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
pub(crate) const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
pub(crate) const ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
pub(crate) const USER_URL: &str = "https://api.github.com/user";

pub(crate) fn signed_out() -> PluginError {
    PluginError::authentication("Sign in to GitHub Copilot again.")
}

/// A device sign-in waiting on its user: the code GitHub gave, asked about
/// no more often than GitHub allows.
pub(crate) struct Pending {
    pub device_code: String,
    pub interval: Duration,
    pub next: Instant,
    pub expires: Instant,
}

/// What the device flow's start answered: the page to open, the code to
/// type there and the sign-in to wait on.
pub(crate) struct Started {
    pub url: String,
    pub code: String,
    pub pending: Pending,
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

fn form(pairs: &[(&str, &str)]) -> Vec<u8> {
    let mut form = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in pairs {
        form.append_pair(key, value);
    }
    form.finish().into_bytes()
}

fn oauth_request(url: &str, body: Vec<u8>) -> HttpRequest {
    crate::copilot::editor_headers(HttpRequest::new(HttpMethod::Post, url))
        .with_header("accept", "application/json")
        .with_header("content-type", "application/x-www-form-urlencoded")
        .with_body(body)
}

pub(crate) async fn start(client: &dyn HttpClient) -> PluginResult<Started> {
    let request = oauth_request(
        DEVICE_CODE_URL,
        form(&[("client_id", CLIENT_ID), ("scope", "read:user")]),
    );
    let (status, value) = json(client, request, Duration::from_secs(30)).await?;
    let text = |key: &str| value[key].as_str().unwrap_or_default().to_owned();
    let (device_code, code, url) = (
        text("device_code"),
        text("user_code"),
        text("verification_uri"),
    );
    if !(200..300).contains(&status) || device_code.is_empty() || code.is_empty() {
        return Err(PluginError::transport(explain(
            "GitHub did not start a sign-in",
            status,
            &value,
        )));
    }
    let now = Instant::now();
    let interval = Duration::from_secs(value["interval"].as_u64().unwrap_or(5).max(1));
    let lifetime = Duration::from_secs(value["expires_in"].as_u64().unwrap_or(900));
    Ok(Started {
        url: if url.is_empty() {
            "https://github.com/login/device".into()
        } else {
            url
        },
        code,
        pending: Pending {
            device_code,
            interval,
            next: now + interval,
            expires: now + lifetime,
        },
    })
}

/// Asks GitHub once whether the user typed the code: the GitHub token once
/// they did, none while they have not.
pub(crate) async fn poll(
    client: &dyn HttpClient,
    pending: &mut Pending,
) -> PluginResult<Option<String>> {
    let now = Instant::now();
    if now >= pending.expires {
        return Err(PluginError::authentication(
            "The GitHub sign-in code expired. Sign in again.",
        ));
    }
    if now < pending.next {
        return Ok(None);
    }
    let request = oauth_request(
        ACCESS_TOKEN_URL,
        form(&[
            ("client_id", CLIENT_ID),
            ("device_code", &pending.device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ]),
    );
    let (status, value) = json(client, request, Duration::from_secs(30)).await?;
    if let Some(token) = value["access_token"]
        .as_str()
        .filter(|token| !token.is_empty())
    {
        return Ok(Some(token.to_owned()));
    }
    match value["error"].as_str().unwrap_or_default() {
        "authorization_pending" => {}
        "slow_down" => {
            let asked = value["interval"].as_u64().map(Duration::from_secs);
            pending.interval = asked.unwrap_or(pending.interval + Duration::from_secs(5));
        }
        "expired_token" => {
            return Err(PluginError::authentication(
                "The GitHub sign-in code expired. Sign in again.",
            ))
        }
        "access_denied" => {
            return Err(PluginError::authentication(
                "The GitHub sign-in was cancelled.",
            ))
        }
        _ => {
            return Err(PluginError::authentication(explain(
                "GitHub refused the sign-in",
                status,
                &value,
            )))
        }
    }
    pending.next = Instant::now() + pending.interval;
    Ok(None)
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

/// What GitHub said went wrong, after what was being done.
pub(crate) fn explain(doing: &str, status: u16, value: &Value) -> String {
    let message = ["error_description", "message", "error"]
        .iter()
        .find_map(|key| value[*key].as_str().filter(|text| !text.is_empty()));
    match message {
        Some(message) => format!("{doing} (HTTP {status}): {message}"),
        None => format!("{doing} (HTTP {status})."),
    }
}

#[cfg(test)]
#[path = "../test/github.test.rs"]
mod tests;
