//! The transferable OAuth record exported by CLIProxyAPI. Importing takes a
//! private copy; neither a path nor a live reference to CPA belongs in it.
use crate::{xai_auth::XAI_COMPAT_OAUTH_CLIENT_ID, XAI_OIDC_ISSUER};
use base64::Engine;
use nora_ai::{OAuthCredentials, OAuthProviderId};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fmt};

pub const IMPORT_NOTICE: &str = "Stop using this account in CPA before importing it. Both clients rotate the same refresh token.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportError {
    AccountType,
    Credentials,
    Timestamps,
    TokenAudience,
}
impl fmt::Display for ImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AccountType => "CPA file is not an enabled xAI OAuth account.",
            Self::Credentials => "CPA xAI account is missing its tokens or identity.",
            Self::Timestamps => "CPA xAI account has invalid token timestamps.",
            Self::TokenAudience => "CPA token is not for the Grok Build xAI account.",
        })
    }
}
impl std::error::Error for ImportError {}

pub fn credentials(source: &Value) -> Result<OAuthCredentials, ImportError> {
    if !source.is_object()
        || source["type"] != "xai"
        || source["auth_kind"] != "oauth"
        || source["disabled"] != false
    {
        return Err(ImportError::AccountType);
    }
    let access = token(&source["access_token"])?;
    let refresh = token(&source["refresh_token"])?;
    let user = identity(&source["sub"])?;
    let email = identity(&source["email"])?;
    let created = timestamp(&source["last_refresh"])?;
    let expires = timestamp(&source["expired"])?;
    let claims = claims(access).ok_or(ImportError::TokenAudience)?;
    let audience = match &claims["aud"] {
        Value::String(audience) => audience == XAI_COMPAT_OAUTH_CLIENT_ID,
        Value::Array(audiences) => audiences
            .iter()
            .any(|audience| audience == XAI_COMPAT_OAUTH_CLIENT_ID),
        _ => false,
    };
    if claims["iss"] != XAI_OIDC_ISSUER || !audience {
        return Err(ImportError::TokenAudience);
    }
    Ok(OAuthCredentials::new(
        OAuthProviderId::XAI,
        access,
        Some(refresh.into()),
        Some(expires),
        BTreeMap::from([
            ("userId".into(), json!(user).into()),
            ("email".into(), json!(email).into()),
            ("oidcIssuer".into(), json!(XAI_OIDC_ISSUER).into()),
            (
                "oidcClientId".into(),
                json!(XAI_COMPAT_OAUTH_CLIENT_ID).into(),
            ),
            ("createdAt".into(), json!(created).into()),
            (
                "accountIdentity".into(),
                json!({"issuer":XAI_OIDC_ISSUER,"subject":user,"scope":"[\"\",\"\"]"}).into(),
            ),
        ]),
    ))
}

fn token(value: &Value) -> Result<&str, ImportError> {
    value
        .as_str()
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 256 * 1024
                && !value
                    .chars()
                    .any(|ch| ch.is_control() || ch.is_whitespace())
        })
        .ok_or(ImportError::Credentials)
}
fn identity(value: &Value) -> Result<&str, ImportError> {
    value
        .as_str()
        .filter(|value| {
            !value.trim().is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
        })
        .ok_or(ImportError::Credentials)
}
fn timestamp(value: &Value) -> Result<i64, ImportError> {
    let value = value.as_str().ok_or(ImportError::Timestamps)?;
    if let Ok(time) = chrono::DateTime::parse_from_rfc3339(value)
        .or_else(|_| chrono::DateTime::parse_from_str(value, "%Y-%m-%dT%H:%M%#z"))
        .or_else(|_| chrono::DateTime::parse_from_rfc2822(value))
    {
        return Ok(time.timestamp_millis());
    }
    // Date.parse treats ISO date-only forms as UTC, but an ISO time without
    // an offset as local time. CPA normally writes complete RFC3339 strings.
    let date = match value.len() {
        4 => format!("{value}-01-01"),
        7 => format!("{value}-01"),
        10 => value.into(),
        _ => String::new(),
    };
    if let Ok(date) = chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d") {
        if let Some(time) = date.and_hms_opt(0, 0, 0) {
            return Ok(time.and_utc().timestamp_millis());
        }
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M"] {
        if let Ok(time) = chrono::NaiveDateTime::parse_from_str(value, format) {
            if let Some(time) = time.and_local_timezone(chrono::Local).earliest() {
                return Ok(time.timestamp_millis());
            }
        }
    }
    Err(ImportError::Timestamps)
}
fn claims(token: &str) -> Option<Value> {
    let part = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(part)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(part))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}
