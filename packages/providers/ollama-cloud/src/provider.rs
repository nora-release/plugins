use async_trait::async_trait;
use nora_ai::{HttpClient, ResolvedProviderAuth};
use nora_provider::ProviderAdapter;
use nora_provider_quota::{fetch, unsupported_origin};
use serde_json::Value;

#[path = "usage.rs"]
mod usage;

#[derive(Debug)]
pub struct OllamaCloud;

#[async_trait]
impl ProviderAdapter for OllamaCloud {
    fn id(&self) -> Option<&'static str> {
        Some("ollama-cloud")
    }

    async fn usage(
        &self,
        auth: &ResolvedProviderAuth,
        _account: Option<&str>,
        client: &dyn HttpClient,
    ) -> Result<Value, String> {
        let payload = fetch(
            auth,
            "ollama.com",
            "https://ollama.com/api/balance",
            true,
            client,
        )
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
