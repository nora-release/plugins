//! GitHub's account identity, shared by device sign-in and local Copilot sources.

use nora_ai::{HttpClient, HttpMethod, HttpRequest, JsonValue, OAuthCredentials, OAuthError};
use serde_json::{json, Value};

pub const IDENTITY_KEY: &str = "accountIdentity";

pub fn authentication_rejected(error: &OAuthError) -> bool {
    matches!(error, OAuthError::Missing(_) | OAuthError::RefreshFailed(_))
}

/// Only GitHub.com and GitHub Enterprise Cloud serve Copilot's GitHub API.
pub fn normalize_host(value: &str) -> Option<String> {
    let value = value.trim().trim_end_matches('/');
    let host = value
        .strip_prefix("https://")
        .unwrap_or(value)
        .to_ascii_lowercase();
    if host.is_empty() || host == "github.com" {
        return Some(String::new());
    }
    let tenant = host.strip_suffix(".ghe.com")?;
    (!tenant.is_empty()
        && tenant.len() <= 63
        && !tenant.starts_with('-')
        && !tenant.ends_with('-')
        && tenant
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'))
    .then_some(host)
}

pub fn issuer(host: &str) -> String {
    format!(
        "https://{}",
        if host.is_empty() { "github.com" } else { host }
    )
}

pub fn api_url(host: &str, path: &str) -> String {
    format!(
        "https://api.{}{path}",
        if host.is_empty() { "github.com" } else { host }
    )
}

pub fn account_name(login: &str, host: &str) -> String {
    if host.is_empty() {
        login.into()
    } else {
        format!("{login}@{host}")
    }
}

pub fn host(credentials: &OAuthCredentials) -> Result<String, OAuthError> {
    let value = credentials
        .extras
        .get("githubHost")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    normalize_host(value)
        .ok_or_else(|| OAuthError::InvalidResponse("The GitHub account host is invalid.".into()))
}

pub fn has_identity(credentials: &OAuthCredentials) -> bool {
    credentials
        .extras
        .get(IDENTITY_KEY)
        .is_some_and(|identity| {
            identity
                .as_value()
                .get("issuer")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty())
                && identity
                    .as_value()
                    .get("subject")
                    .and_then(Value::as_str)
                    .is_some_and(|value| !value.is_empty())
        })
}

pub async fn enrich(
    credentials: &mut OAuthCredentials,
    client: &dyn HttpClient,
) -> Result<(), OAuthError> {
    let host = host(credentials)?;
    enrich_at(credentials, client, &api_url(&host, "/user"), &host).await
}

/// `user_url` is explicitly configured by the device-flow owner, never taken
/// from an upstream payload or a saved arbitrary endpoint.
pub async fn enrich_at(
    credentials: &mut OAuthCredentials,
    client: &dyn HttpClient,
    user_url: &str,
    host: &str,
) -> Result<(), OAuthError> {
    let token = credentials
        .refresh_token
        .as_deref()
        .filter(|token| !token.is_empty())
        .or_else(|| {
            (!credentials.access_token.is_empty()).then_some(credentials.access_token.as_str())
        })
        .ok_or_else(|| {
            OAuthError::InvalidResponse("The GitHub sign-in has no account token.".into())
        })?;
    let response = client
        .request(
            HttpRequest::new(HttpMethod::Get, user_url)
                .with_header("accept", "application/json")
                .with_header("authorization", format!("Bearer {token}"))
                .with_header("user-agent", "Nora-Copilot")
                .with_header("x-github-api-version", "2022-11-28"),
        )
        .await?;
    if response.status != 200 {
        let message = format!(
            "GitHub account verification returned HTTP {}.",
            response.status
        );
        return Err(if matches!(response.status, 401 | 403) {
            OAuthError::RefreshFailed(message)
        } else {
            OAuthError::Transport(message)
        });
    }
    if response.body.len() > 1_048_576 {
        return Err(OAuthError::InvalidResponse(
            "GitHub's account response is too large.".into(),
        ));
    }
    let user: Value = serde_json::from_slice(&response.body)
        .map_err(|_| OAuthError::InvalidResponse("GitHub's account response is invalid.".into()))?;
    apply_profile(credentials, &user, host)
}

pub fn apply_profile(
    credentials: &mut OAuthCredentials,
    user: &Value,
    host: &str,
) -> Result<(), OAuthError> {
    let login = user
        .get("login")
        .and_then(Value::as_str)
        .filter(|login| {
            !login.is_empty()
                && login.len() <= 100
                && login
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
        .ok_or_else(|| {
            OAuthError::InvalidResponse("GitHub did not say whose account this is.".into())
        })?;
    let host = normalize_host(host)
        .ok_or_else(|| OAuthError::InvalidResponse("The GitHub account host is invalid.".into()))?;
    let subject = user
        .get("id")
        .and_then(|id| {
            id.as_u64()
                .filter(|id| *id > 0)
                .map(|id| id.to_string())
                .or_else(|| {
                    id.as_str()
                        .filter(|id| {
                            !id.is_empty()
                                && id.len() <= 32
                                && id.bytes().all(|byte| byte.is_ascii_digit())
                        })
                        .map(str::to_owned)
                })
        })
        .unwrap_or_else(|| format!("login:{}", login.to_ascii_lowercase()));
    credentials.extras.insert(
        IDENTITY_KEY.into(),
        JsonValue::from(json!({"issuer":issuer(&host),"subject":subject})),
    );
    credentials
        .extras
        .insert("githubLogin".into(), JsonValue::string(login));
    credentials
        .extras
        .insert("githubHost".into(), JsonValue::string(&host));
    credentials.extras.insert(
        "accountId".into(),
        JsonValue::string(account_name(login, &host)),
    );
    credentials
        .extras
        .insert("name".into(), JsonValue::string(account_name(login, &host)));
    if let Some(email) = user.get("email").and_then(Value::as_str).filter(|email| {
        email.len() <= 512 && email.contains('@') && !email.chars().any(char::is_whitespace)
    }) {
        credentials
            .extras
            .insert("email".into(), JsonValue::string(email));
    }
    Ok(())
}

#[cfg(test)]
#[path = "../test/copilot_identity.test.rs"]
mod tests;
