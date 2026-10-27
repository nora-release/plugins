pub mod tls;
use nora_provider_plugin_sdk::{NetworkProxy, PluginError, PluginResult};

/// Retain the pinned SDK's proxy and redirect policy, adding platform trust to
/// Rustls without changing the TLS versions supported by provider requests.
pub fn http_client_builder(proxy: Option<&NetworkProxy>) -> PluginResult<reqwest::ClientBuilder> {
    platform_trust(nora_provider_plugin_sdk::http_client_builder(proxy)?)
}

pub fn http_client(proxy: Option<&NetworkProxy>) -> PluginResult<reqwest::Client> {
    build(http_client_builder(proxy)?)
}

/// The pinned AI client's default transport uses reqwest's own proxy and
/// redirect policies. Keep those defaults for OAuthManager's standalone use.
pub fn default_http_client() -> PluginResult<reqwest::Client> {
    build(platform_trust(reqwest::Client::builder())?)
}

fn platform_trust(builder: reqwest::ClientBuilder) -> PluginResult<reqwest::ClientBuilder> {
    #[cfg(target_os = "macos")]
    let builder =
        builder.use_preconfigured_tls(tls::http_client_config(std::iter::empty()).map_err(
            |_| PluginError::transport("The provider TLS client could not be initialized."),
        )?);
    Ok(builder)
}

fn build(builder: reqwest::ClientBuilder) -> PluginResult<reqwest::Client> {
    builder
        .build()
        .map_err(|_| PluginError::transport("The provider HTTP client could not be initialized."))
}

#[cfg(all(test, target_os = "macos", feature = "http2"))]
#[path = "../test/http2.test.rs"]
mod tests;

#[cfg(test)]
#[path = "../test/default_policy.test.rs"]
mod default_policy_tests;
