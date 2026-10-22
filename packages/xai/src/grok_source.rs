use nora_ai::{OAuthCredentials, ResolvedProviderAuth};
use nora_provider::{auth::base_auth, ProviderAdapter};
use nora_provider_plugin_sdk::NetworkProxy;

pub(crate) use nora_provider_auth::grok_source::{
    bind_current_account_with_network, client_version, is_bound,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentAuthResolveReason {
    Initial,
    Refresh,
}

pub(crate) async fn resolve(
    credentials: &OAuthCredentials,
    reason: AgentAuthResolveReason,
    proxy: Option<&NetworkProxy>,
) -> Option<ResolvedProviderAuth> {
    let transient = nora_provider_auth::grok_source::resolve_credentials_with_network(
        credentials,
        reason == AgentAuthResolveReason::Refresh,
        proxy,
    )
    .await
    .ok()?;
    let mut auth = base_auth("xai", &transient);
    super::Xai.enrich_auth(&transient, &mut auth);
    Some(auth)
}
