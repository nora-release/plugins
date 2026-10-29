use nora_ai::{OAuthCredentials, ResolvedProviderAuth};
use nora_provider::{auth::base_auth, ProviderAdapter};
use nora_provider_plugin_sdk::{NetworkProxy, PluginError, PluginResult};

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
) -> PluginResult<ResolvedProviderAuth> {
    let transient = nora_provider_auth::grok_source::resolve_credentials_with_network(
        credentials,
        reason == AgentAuthResolveReason::Refresh,
        proxy,
    )
    .await
    .map_err(source_error)?;
    let mut auth = base_auth("xai", &transient);
    super::Xai::default().enrich_auth(&transient, &mut auth);
    Ok(auth)
}

fn source_error(error: nora_provider_auth::grok_source::SourceError) -> PluginError {
    use nora_provider_auth::grok_source::SourceError;
    match error {
        SourceError::InvalidSource | SourceError::InvalidCredentials | SourceError::NotSignedIn =>
            PluginError::authentication("The Grok Build sign-in is no longer available. Sign in with Grok Build again."),
        SourceError::AccountChanged => PluginError::authentication("Grok Build is using a different account. Reconnect this account in Nora."),
        SourceError::InvalidNetworkProxy => PluginError::invalid("The network proxy configuration is invalid."),
        SourceError::ReadFailed => PluginError::transport("The Grok Build sign-in could not be read. It may be updating; try again in a moment."),
        SourceError::CliUnavailable => PluginError::transport("Grok Build is unavailable to renew this sign-in. Open Grok Build and try again."),
        SourceError::RefreshFailed | SourceError::RenewalUnchanged | SourceError::Expired | SourceError::TimedOut =>
            PluginError::transport("Grok Build could not renew this sign-in. Open Grok Build and send it a message, then try again."),
    }
}

#[cfg(test)]
#[path = "../test/grok_source.test.rs"]
mod tests;
