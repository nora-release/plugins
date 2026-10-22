use async_trait::async_trait;
use nora_ai::{HttpClient, OAuthCredentials, ResolvedProviderAuth};
use nora_provider::ProviderAdapter;
use nora_provider_plugin_sdk::PluginResult;
use serde_json::Value;

#[path = "../../zhipu/src/usage.rs"]
mod usage;

#[path = "../../zhipu/src/team.rs"]
mod team;

#[derive(Debug)]
pub struct Zai;

#[async_trait]
impl ProviderAdapter for Zai {
    fn id(&self) -> Option<&'static str> {
        Some("zai")
    }

    fn configure_api_credentials(
        &self,
        inputs: &Value,
        credentials: &mut OAuthCredentials,
    ) -> PluginResult<()> {
        team::configure(inputs, credentials)
    }
    fn enrich_auth(&self, credentials: &OAuthCredentials, auth: &mut ResolvedProviderAuth) {
        team::enrich(credentials, auth);
    }

    async fn usage(
        &self,
        auth: &ResolvedProviderAuth,
        _account: Option<&str>,
        client: &dyn HttpClient,
    ) -> Result<Value, String> {
        team::usage(
            auth,
            "api.z.ai",
            "https://api.z.ai/api/monitor/usage/quota/limit",
            client,
        )
        .await
    }
}

#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
