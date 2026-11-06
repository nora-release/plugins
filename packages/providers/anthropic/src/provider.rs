#[path = "fast.rs"]
pub(crate) mod fast;

use async_trait::async_trait;
use nora_ai::{HttpClient, ResolvedProviderAuth};
use nora_provider::{adapter::is_oauth, ProviderAdapter};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::Value;
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
    fn account_import_kind(&self) -> Option<nora_provider::account_import::LoginKind> {
        Some(nora_provider::account_import::LoginKind::Claude)
    }
    fn remote_models(&self, credentials: Option<&Value>) -> bool {
        !is_oauth(credentials)
    }
    async fn prepare_fetch(
        &self,
        _definition: &nora_provider::definition::Definition,
        credentials: &Value,
        _target: &mut Url,
        _headers: &mut BTreeMap<String, String>,
        _body: &mut Option<Vec<u8>>,
        _auth: &mut ResolvedProviderAuth,
    ) -> PluginResult<()> {
        if is_oauth(Some(credentials)) {
            return Err(PluginError::new(
                "subscription_requires_claude_cli",
                "Claude subscriptions must use the installed Claude Code transport.",
            ));
        }
        Ok(())
    }
    fn finalize_fetch(
        &self,
        credentials: &Value,
        target: &Url,
        headers: &mut BTreeMap<String, String>,
        body: &mut Option<Vec<u8>>,
    ) -> PluginResult<()> {
        if is_oauth(Some(credentials)) {
            return Ok(());
        }
        fast::finalize(target, headers, body);
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
