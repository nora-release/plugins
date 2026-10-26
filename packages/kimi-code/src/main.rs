use async_trait::async_trait;
use nora_ai::{HttpClient, ResolvedProviderAuth};
use nora_provider::ProviderAdapter;
use nora_provider_quota::{fetch, unsupported_origin};
use serde_json::Value;

#[path = "../../kimi-coding/src/search.rs"]
mod search;
#[path = "../../kimi-coding/src/usage.rs"]
mod usage;
#[derive(Debug)]
struct KimiGlobal;
#[async_trait]
impl ProviderAdapter for KimiGlobal {
    fn supports_web_search(&self) -> bool {
        true
    }
    async fn web_search(
        &self,
        auth: &ResolvedProviderAuth,
        query: &str,
        client: &dyn HttpClient,
    ) -> nora_provider_plugin_sdk::PluginResult<Value> {
        search::search(auth, query, client).await
    }
    fn id(&self) -> Option<&'static str> {
        Some("kimi-code")
    }
    async fn usage(
        &self,
        auth: &ResolvedProviderAuth,
        _account: Option<&str>,
        client: &dyn HttpClient,
    ) -> Result<Value, String> {
        let payload = fetch(
            auth,
            "api.kimi.ai",
            "https://api.kimi.ai/coding/v1/usages",
            true,
            client,
        )
        .await
        .map_err(|error| error.to_string())?;
        match payload {
            Some(value) => usage::parse(&value),
            None => Ok(unsupported_origin()),
        }
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() {
    nora_provider::run(KimiGlobal).await;
}
