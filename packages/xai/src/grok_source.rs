use nora_ai::{OAuthCredentials, ResolvedProviderAuth};
use nora_provider::{auth::base_auth, ProviderAdapter};

pub(crate) use nora_provider_auth::grok_source::{bind_current_account, client_version, is_bound};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentAuthResolveReason {
    Initial,
    Refresh,
}

pub(crate) async fn resolve(
    credentials: &OAuthCredentials,
    reason: AgentAuthResolveReason,
) -> Option<ResolvedProviderAuth> {
    let transient = nora_provider_auth::grok_source::resolve_credentials(
        credentials,
        reason == AgentAuthResolveReason::Refresh,
    )
    .await
    .ok()?;
    let mut auth = base_auth("xai", &transient);
    super::Xai.enrich_auth(&transient, &mut auth);
    Some(auth)
}
