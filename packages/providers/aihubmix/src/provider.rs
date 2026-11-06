use async_trait::async_trait;
use nora_ai::{HttpClient, JsonValue, OAuthCredentials, ResolvedProviderAuth};
use nora_provider::ProviderAdapter;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use nora_provider_quota::{fetch, unsupported_origin};
use serde_json::Value;

#[path = "usage.rs"]
mod usage;

#[derive(Debug)]
pub struct AiHubMix;

#[async_trait]
impl ProviderAdapter for AiHubMix {
    fn id(&self) -> Option<&'static str> {
        Some("aihubmix")
    }

    fn configure_api_credentials(
        &self,
        inputs: &Value,
        credentials: &mut OAuthCredentials,
    ) -> PluginResult<()> {
        if let Some(token) = inputs
            .get("balanceToken")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|token| !token.is_empty())
        {
            if token.len() > 8192 || token.contains(['\r', '\n']) {
                return Err(PluginError::invalid(
                    "The account balance token is invalid.",
                ));
            }
            credentials
                .extras
                .insert("balanceToken".into(), JsonValue::string(token));
        }
        Ok(())
    }
    fn enrich_auth(&self, credentials: &OAuthCredentials, auth: &mut ResolvedProviderAuth) {
        if let Some(token) = credentials
            .extras
            .get("balanceToken")
            .and_then(|value| value.as_str())
        {
            auth.metadata
                .insert("balanceToken".into(), JsonValue::string(token));
        }
    }
    async fn usage(
        &self,
        auth: &ResolvedProviderAuth,
        _account: Option<&str>,
        client: &dyn HttpClient,
    ) -> Result<Value, String> {
        let mut query = auth.clone();
        let endpoint = if let Some(token) = auth
            .metadata
            .get("balanceToken")
            .and_then(|value| value.as_str())
        {
            query.token = Some(token.into());
            "https://aihubmix.com/api/user/self"
        } else {
            "https://aihubmix.com/dashboard/billing/remain"
        };
        let payload = fetch(&query, "aihubmix.com", endpoint, true, client)
            .await
            .map_err(|error| error.to_string())?;
        let Some(payload) = payload else {
            return Ok(unsupported_origin());
        };
        usage::parse(&payload)
    }
}

#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
