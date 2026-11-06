//! Volcengine Ark plan allowance adapter, adapted from Magpie (MIT).
use async_trait::async_trait;
use nora_ai::{HttpClient, JsonValue, OAuthCredentials, ResolvedProviderAuth};
use nora_provider::ProviderAdapter;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::Value;

#[derive(Debug)]
pub struct Volcengine;

#[async_trait]
impl ProviderAdapter for Volcengine {
    fn id(&self) -> Option<&'static str> {
        Some("volcengine")
    }
    fn configure_api_credentials(
        &self,
        inputs: &Value,
        credentials: &mut OAuthCredentials,
    ) -> PluginResult<()> {
        for key in ["accessKeyID", "secretAccessKey"] {
            let Some(raw) = inputs.get(key).filter(|value| !value.is_null()) else {
                continue;
            };
            let value = raw
                .as_str()
                .ok_or_else(|| PluginError::invalid("The Volcengine access key is invalid."))?
                .trim();
            if value.len() > 4096 || value.chars().any(char::is_control) {
                return Err(PluginError::invalid(
                    "The Volcengine access key is invalid.",
                ));
            }
            if !value.is_empty() {
                credentials
                    .extras
                    .insert(key.into(), JsonValue::string(value));
            }
        }
        Ok(())
    }
    fn enrich_auth(&self, credentials: &OAuthCredentials, auth: &mut ResolvedProviderAuth) {
        for key in ["accessKeyID", "secretAccessKey"] {
            if let Some(value) = credentials.extras.get(key) {
                auth.metadata.insert(key.into(), value.clone());
            }
        }
    }
    async fn usage(
        &self,
        auth: &ResolvedProviderAuth,
        _account_id: Option<&str>,
        client: &dyn HttpClient,
    ) -> Result<Value, String> {
        super::usage::fetch(auth, client).await
    }
}

#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
