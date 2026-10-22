use async_trait::async_trait;
use nora_ai::{HttpClient, ResolvedProviderAuth};
use nora_provider::ProviderAdapter;
use nora_provider_quota::{fetch, unsupported_origin};
use serde_json::Value;

#[path = "usage.rs"]
mod usage;

#[derive(Debug)]
pub struct KimiCoding;

#[async_trait]
impl ProviderAdapter for KimiCoding {
    fn id(&self) -> Option<&'static str> {
        Some("kimi-coding")
    }

    async fn usage(
        &self,
        auth: &ResolvedProviderAuth,
        _account: Option<&str>,
        client: &dyn HttpClient,
    ) -> Result<Value, String> {
        let alternative = auth
            .base_url
            .as_deref()
            .and_then(|base| url::Url::parse(base).ok())
            .is_some_and(|base| base.host_str() == Some("api.kimi.ai"));
        let (host, endpoint) = if alternative {
            ("api.kimi.ai", "https://api.kimi.ai/coding/v1/usages")
        } else {
            ("api.kimi.com", "https://api.kimi.com/coding/v1/usages")
        };
        let payload = fetch(auth, host, endpoint, true, client)
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
