use async_trait::async_trait;
use nora_ai::{
    HttpClient, OAuthCredentials, OAuthError, OAuthProvider, OAuthProviderId, ResolvedProviderAuth,
};
use nora_provider::{adapter::is_oauth, ProviderAdapter};
use nora_provider_auth::{AnthropicOAuthProvider, OAuthLogin, OAuthLoginCallbacks};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use url::Url;

#[path = "usage.rs"]
pub mod usage;

#[derive(Debug)]
pub struct Anthropic;

#[async_trait]
impl ProviderAdapter for Anthropic {
    fn id(&self) -> Option<&'static str> {
        Some("anthropic")
    }
    async fn login(
        &self,
        callbacks: OAuthLoginCallbacks,
        client: &dyn HttpClient,
    ) -> Result<OAuthCredentials, OAuthError> {
        OAuthLogin::login_anthropic(callbacks, client).await
    }
    async fn refresh(
        &self,
        credentials: &OAuthCredentials,
        client: &dyn HttpClient,
    ) -> PluginResult<OAuthCredentials> {
        AnthropicOAuthProvider::default()
            .refresh(credentials, client)
            .await
            .map_err(|_| {
                PluginError::authentication(
                    "The provider account could not be renewed. Sign in again.",
                )
            })
    }
    fn enrich_auth(&self, credentials: &OAuthCredentials, auth: &mut ResolvedProviderAuth) {
        if credentials.provider == OAuthProviderId::Anthropic {
            auth.headers
                .insert("anthropic-beta".into(), "oauth-2025-04-20".into());
        }
    }
    // Claude subscription accounts do not provide a public live model list.
    fn remote_models(&self, credentials: Option<&Value>) -> bool {
        !is_oauth(credentials)
    }
    async fn prepare_fetch(
        &self,
        credentials: &Value,
        _target: &mut Url,
        headers: &mut BTreeMap<String, String>,
        body: &mut Option<Vec<u8>>,
        auth: &mut ResolvedProviderAuth,
    ) -> PluginResult<()> {
        if !is_oauth(Some(credentials)) {
            return Ok(());
        }
        if let Some(bytes) = body.as_ref() {
            let mut payload: Value = serde_json::from_slice(bytes)
                .map_err(|_| PluginError::invalid("The Anthropic request body is invalid."))?;
            let mut system = vec![
                json!({"type":"text","text":"You are Claude Code, Anthropic's official CLI for Claude."}),
            ];
            match payload.get("system").cloned() {
                Some(Value::Array(values)) => system.extend(values),
                Some(Value::String(text)) if !text.is_empty() => {
                    system.push(json!({"type":"text","text":text}))
                }
                _ => {}
            }
            payload["system"] = json!(system);
            *body = Some(
                serde_json::to_vec(&payload)
                    .map_err(|_| PluginError::invalid("Cannot encode the Anthropic request."))?,
            );
        }
        if let Some(previous) = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("anthropic-beta"))
            .map(|(_, value)| value.clone())
        {
            if !previous
                .split(',')
                .any(|value| value.trim() == "oauth-2025-04-20")
            {
                auth.headers.insert(
                    "anthropic-beta".into(),
                    format!("{previous},oauth-2025-04-20"),
                );
            }
        }
        Ok(())
    }
    async fn usage(
        &self,
        auth: &ResolvedProviderAuth,
        account_id: Option<&str>,
        client: &dyn HttpClient,
    ) -> Result<Value, String> {
        usage::fetch(auth, account_id, client).await
    }
}
