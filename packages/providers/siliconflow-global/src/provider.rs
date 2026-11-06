use async_trait::async_trait;
use nora_ai::{HttpClient, ResolvedProviderAuth};
use nora_provider::ProviderAdapter;
use nora_provider_quota::{fetch, unsupported_origin};
use serde_json::Value;

#[path = "usage.rs"]
mod usage;

#[derive(Debug)]
pub struct SiliconFlowGlobal;

#[async_trait]
impl ProviderAdapter for SiliconFlowGlobal {
    fn id(&self) -> Option<&'static str> {
        Some("siliconflow-global")
    }

    async fn usage(
        &self,
        auth: &ResolvedProviderAuth,
        _account: Option<&str>,
        client: &dyn HttpClient,
    ) -> Result<Value, String> {
        let payload = fetch(auth, "api.siliconflow.com", "https://api.siliconflow.com/v1/user/info", true, client).await.map_err(|error| match error { nora_provider_quota::QuotaError::Status(410) => "SiliconFlow retired its balance query and has no replacement API; check the provider console for balances and vouchers.".into(), _ => error.to_string() })?;
        let Some(payload) = payload else {
            return Ok(unsupported_origin());
        };
        usage::parse(&payload)
    }
}

#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
