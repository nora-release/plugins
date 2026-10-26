use crate::xai_auth::{xai_grok_client_version, xai_oauth_client_id, XAI_CLIENT_VERSION_HEADER};
use async_trait::async_trait;
use base64::Engine;
use nora_ai::http::{HttpClient, HttpMethod, HttpRequest};
use nora_ai::json_value::JsonValue;
use nora_ai::oauth::{
    decode_token_response, expires_from_now, url_encoded_form, OAuthCredentials, OAuthError,
    OAuthProvider, OAuthProviderId, OAuthTokenResponse,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

const EARLY_REFRESH_MILLIS: i64 = 5 * 60 * 1_000;

// MARK: - Anthropic

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnthropicOAuthProvider {
    pub token_url: String,
    pub client_id: String,
}

impl Default for AnthropicOAuthProvider {
    fn default() -> Self {
        Self {
            token_url: "https://platform.claude.com/v1/oauth/token".to_string(),
            client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e".to_string(),
        }
    }
}

impl AnthropicOAuthProvider {
    pub fn new(token_url: impl Into<String>, client_id: impl Into<String>) -> Self {
        Self {
            token_url: token_url.into(),
            client_id: client_id.into(),
        }
    }
}

#[async_trait]
impl OAuthProvider for AnthropicOAuthProvider {
    fn id(&self) -> &str {
        "anthropic"
    }

    fn name(&self) -> &str {
        "Anthropic (Claude Pro/Max)"
    }

    async fn refresh(
        &self,
        credentials: &OAuthCredentials,
        client: &dyn HttpClient,
    ) -> Result<OAuthCredentials, OAuthError> {
        let refresh_token = require_refresh_token(credentials, self.id())?;
        let body = serde_json::to_vec(&json!({
            "grant_type": "refresh_token",
            "client_id": self.client_id,
            "refresh_token": refresh_token,
        }))
        .map_err(|error| OAuthError::InvalidResponse(error.to_string()))?;

        let response = client
            .request(
                HttpRequest::new(HttpMethod::Post, self.token_url.clone())
                    .with_header("content-type", "application/json")
                    .with_header("accept", "application/json")
                    .with_body(body),
            )
            .await?;
        if response.status >= 400 {
            return Err(OAuthError::RefreshFailed(format!(
                "anthropic {}: {}",
                response.status,
                String::from_utf8_lossy(&response.body)
            )));
        }

        let token = decode_token_response(&response.body)?;
        Ok(OAuthCredentials {
            provider: OAuthProviderId::Anthropic,
            access_token: token.access_token,
            refresh_token: Some(
                token
                    .refresh_token
                    .unwrap_or_else(|| refresh_token.to_string()),
            ),
            expires_at: Some(expires_from_now(token.expires_in)),
            extras: credentials.extras.clone(),
        })
    }
}

// MARK: - OpenAI Codex

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenAICodexOAuthProvider {
    pub token_url: String,
    pub client_id: String,
}

impl Default for OpenAICodexOAuthProvider {
    fn default() -> Self {
        Self {
            token_url: "https://auth.openai.com/oauth/token".to_string(),
            client_id: "app_EMoamEEZ73f0CkXaXp7hrann".to_string(),
        }
    }
}

impl OpenAICodexOAuthProvider {
    pub fn new(token_url: impl Into<String>, client_id: impl Into<String>) -> Self {
        Self {
            token_url: token_url.into(),
            client_id: client_id.into(),
        }
    }

    /// Exchanges a refresh token for the token response as OpenAI sends it,
    /// ID token included.
    pub async fn refresh_tokens(
        &self,
        refresh_token: &str,
        client: &dyn HttpClient,
    ) -> Result<OAuthTokenResponse, OAuthError> {
        let form = url_encoded_form(&BTreeMap::from([
            ("client_id".to_string(), self.client_id.clone()),
            ("grant_type".to_string(), "refresh_token".to_string()),
            ("refresh_token".to_string(), refresh_token.to_string()),
        ]));
        let response = client
            .request(
                HttpRequest::new(HttpMethod::Post, self.token_url.clone())
                    .with_header("content-type", "application/x-www-form-urlencoded")
                    .with_header("accept", "application/json")
                    .with_body(form),
            )
            .await?;
        if response.status >= 400 {
            return Err(OAuthError::RefreshFailed(format!(
                "openai-codex {}: {}",
                response.status,
                String::from_utf8_lossy(&response.body)
            )));
        }
        decode_token_response(&response.body)
    }

    /// Extract the ChatGPT account id from the access token JWT claim.
    pub fn extract_account_id_from_jwt(token: &str) -> Option<String> {
        let value = openai_token_claims(token)?;
        value
            .get("https://api.openai.com/auth")?
            .get("chatgpt_account_id")?
            .as_str()
            .map(ToOwned::to_owned)
    }

    /// Reads display metadata only; these claims do not verify an account or authorize a request.
    pub fn profile_extras_from_tokens(
        access_token: &str,
        id_token: Option<&str>,
    ) -> BTreeMap<String, JsonValue> {
        let mut extras = BTreeMap::new();
        let mut subject = None;
        let mut account = None;
        for token in std::iter::once(access_token).chain(id_token) {
            let Some(claims) = openai_token_claims(token) else {
                continue;
            };
            let profile = &claims["https://api.openai.com/profile"];
            let auth = &claims["https://api.openai.com/auth"];
            subject = profile_display_text(auth.get("chatgpt_user_id"))
                .or_else(|| profile_display_text(auth.get("user_id")))
                .or_else(|| profile_display_text(claims.get("sub")))
                .map(str::to_owned)
                .or(subject);
            account = profile_display_text(auth.get("chatgpt_account_id"))
                .map(str::to_owned)
                .or(account);

            if let Some(email) = profile_display_text(claims.get("email"))
                .filter(|email| valid_profile_email(email))
                .or_else(|| {
                    profile_display_text(profile.get("email"))
                        .filter(|email| valid_profile_email(email))
                })
            {
                extras.insert("email".into(), JsonValue::string(email));
            }
            let name = profile_display_text(claims.get("name"))
                .or_else(|| profile_display_text(profile.get("name")))
                .map(str::to_string)
                .or_else(|| {
                    let given = profile_display_text(claims.get("given_name"))?;
                    let family =
                        profile_display_text(claims.get("family_name")).unwrap_or_default();
                    Some(format!("{given} {family}").trim().to_string())
                });
            if let Some(name) = name {
                extras.insert("name".into(), JsonValue::string(name));
            }
            if let Some(plan) = profile_display_text(auth.get("chatgpt_plan_type"))
                .or_else(|| profile_display_text(claims.get("chatgpt_plan_type")))
                .or_else(|| profile_display_text(profile.get("plan")))
            {
                extras.insert("plan".into(), JsonValue::string(plan));
            }
        }
        if let (Some(subject), Some(account)) = (subject, account) {
            extras.insert(
                "accountIdentity".into(),
                serde_json::json!({
                    "issuer":"https://auth.openai.com", "subject":subject, "scope":account
                })
                .into(),
            );
        }
        extras
    }
}

fn openai_token_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    if payload.len() > 256 * 1024 {
        return None;
    }
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload))
        .ok()?;
    serde_json::from_slice(&decoded).ok()
}

fn profile_display_text(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty() && text.len() <= 512 && !text.chars().any(char::is_control))
}

fn valid_profile_email(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.is_empty()
        && !domain.contains('@')
        && !email.chars().any(char::is_whitespace)
}

#[async_trait]
impl OAuthProvider for OpenAICodexOAuthProvider {
    fn id(&self) -> &str {
        "openai-codex"
    }

    fn name(&self) -> &str {
        "ChatGPT Plus/Pro (Codex Subscription)"
    }

    async fn refresh(
        &self,
        credentials: &OAuthCredentials,
        client: &dyn HttpClient,
    ) -> Result<OAuthCredentials, OAuthError> {
        let refresh_token = require_refresh_token(credentials, self.id())?;
        let token = self.refresh_tokens(refresh_token, client).await?;
        let mut extras = credentials.extras.clone();
        extras.extend(Self::profile_extras_from_tokens(
            &token.access_token,
            token.id_token.as_deref(),
        ));
        if let Some(account_id) = Self::extract_account_id_from_jwt(&token.access_token) {
            extras.insert("accountId".to_string(), JsonValue::string(account_id));
        }

        Ok(OAuthCredentials {
            provider: OAuthProviderId::OpenAICodex,
            access_token: token.access_token,
            refresh_token: Some(
                token
                    .refresh_token
                    .unwrap_or_else(|| refresh_token.to_string()),
            ),
            expires_at: Some(expires_from_now(token.expires_in)),
            extras,
        })
    }
}

// MARK: - GitHub Copilot

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitHubCopilotOAuthProvider {
    pub token_url: String,
    pub extra_headers: BTreeMap<String, String>,
}

impl Default for GitHubCopilotOAuthProvider {
    fn default() -> Self {
        Self {
            token_url: "https://api.github.com/copilot_internal/v2/token".to_string(),
            extra_headers: crate::copilot_client::editor_headers(),
        }
    }
}

impl GitHubCopilotOAuthProvider {
    pub fn new(token_url: impl Into<String>, extra_headers: BTreeMap<String, String>) -> Self {
        Self {
            token_url: token_url.into(),
            extra_headers,
        }
    }
}

#[async_trait]
impl OAuthProvider for GitHubCopilotOAuthProvider {
    fn id(&self) -> &str {
        "github-copilot"
    }

    fn name(&self) -> &str {
        "GitHub Copilot"
    }

    async fn refresh(
        &self,
        credentials: &OAuthCredentials,
        client: &dyn HttpClient,
    ) -> Result<OAuthCredentials, OAuthError> {
        let refresh_token = require_refresh_token(credentials, self.id())?;
        let mut request = HttpRequest::new(HttpMethod::Get, self.token_url.clone())
            .with_header("accept", "application/json")
            .with_header("authorization", format!("Bearer {refresh_token}"));
        for (name, value) in &self.extra_headers {
            request = request.with_header(name, value);
        }

        let response = client.request(request).await?;
        if response.status >= 400 {
            let message = format!(
                "GitHub Copilot token exchange returned HTTP {}.",
                response.status
            );
            return Err(if matches!(response.status, 401 | 403) {
                OAuthError::RefreshFailed(message)
            } else {
                OAuthError::Transport(message)
            });
        }

        let value = serde_json::from_slice::<Value>(&response.body).map_err(|_| {
            OAuthError::InvalidResponse("GitHub Copilot returned an invalid token response.".into())
        })?;
        let token = value.get("token").and_then(Value::as_str).ok_or_else(|| {
            OAuthError::InvalidResponse("github-copilot response missing token".to_string())
        })?;
        let expires_at_seconds = value
            .get("expires_at")
            .and_then(value_as_i64)
            .unwrap_or_else(|| nora_ai::oauth::now_millis() / 1_000 + 25 * 60);

        let mut extras = credentials.extras.clone();
        if let Some(api) = value
            .pointer("/endpoints/api")
            .and_then(Value::as_str)
            .filter(|api| !api.is_empty())
        {
            extras.insert("endpoint".to_string(), JsonValue::string(api));
        }

        Ok(OAuthCredentials {
            provider: OAuthProviderId::GitHubCopilot,
            access_token: token.to_string(),
            refresh_token: Some(refresh_token.to_string()),
            expires_at: Some(expires_at_seconds * 1_000 - EARLY_REFRESH_MILLIS),
            extras,
        })
    }
}

// MARK: - xAI

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XaiOAuthProvider {
    pub token_url: String,
    pub client_id: String,
}

impl Default for XaiOAuthProvider {
    fn default() -> Self {
        Self {
            token_url: "https://auth.x.ai/oauth2/token".to_string(),
            client_id: xai_oauth_client_id().to_string(),
        }
    }
}

impl XaiOAuthProvider {
    pub fn new(token_url: impl Into<String>, client_id: impl Into<String>) -> Self {
        Self {
            token_url: token_url.into(),
            client_id: client_id.into(),
        }
    }
}

#[async_trait]
impl OAuthProvider for XaiOAuthProvider {
    fn id(&self) -> &str {
        "xai"
    }

    fn name(&self) -> &str {
        "xAI (SuperGrok)"
    }

    async fn refresh(
        &self,
        credentials: &OAuthCredentials,
        client: &dyn HttpClient,
    ) -> Result<OAuthCredentials, OAuthError> {
        let refresh_token = require_refresh_token(credentials, self.id())?;
        let form = url_encoded_form(&BTreeMap::from([
            ("client_id".to_string(), self.client_id.clone()),
            ("grant_type".to_string(), "refresh_token".to_string()),
            ("refresh_token".to_string(), refresh_token.to_string()),
        ]));

        let response = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            client.request(
                HttpRequest::new(HttpMethod::Post, self.token_url.clone())
                    .with_header("content-type", "application/x-www-form-urlencoded")
                    .with_header("accept", "application/json")
                    .with_header(XAI_CLIENT_VERSION_HEADER, xai_grok_client_version())
                    .with_body(form),
            ),
        )
        .await
        .map_err(|_| OAuthError::Transport("xAI token refresh timed out".to_string()))??;
        if response.status == 403 {
            return Err(OAuthError::RefreshFailed(
                "xAI did not authorize this account session. Sign in again or use an xAI API key instead.".to_string(),
            ));
        }
        if response.status >= 400 {
            return Err(OAuthError::RefreshFailed(format!(
                "xai {}: {}",
                response.status,
                String::from_utf8_lossy(&response.body)
            )));
        }

        let token = decode_token_response(&response.body)?;
        Ok(OAuthCredentials {
            provider: OAuthProviderId::XAI,
            access_token: token.access_token,
            refresh_token: Some(
                token
                    .refresh_token
                    .unwrap_or_else(|| refresh_token.to_string()),
            ),
            expires_at: Some(expires_from_now(token.expires_in)),
            extras: credentials.extras.clone(),
        })
    }
}

// MARK: - Helpers

pub fn default_oauth_providers() -> Vec<Box<dyn OAuthProvider>> {
    vec![
        Box::<AnthropicOAuthProvider>::default(),
        Box::<OpenAICodexOAuthProvider>::default(),
        Box::<GitHubCopilotOAuthProvider>::default(),
        Box::<XaiOAuthProvider>::default(),
    ]
}

fn require_refresh_token<'a>(
    credentials: &'a OAuthCredentials,
    provider_id: &str,
) -> Result<&'a str, OAuthError> {
    credentials
        .refresh_token
        .as_deref()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            OAuthError::InvalidResponse(format!("{provider_id} credentials missing refresh token"))
        })
}

fn value_as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_f64().map(|value| value as i64))
}
