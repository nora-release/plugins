pub mod tls;
use nora_provider_plugin_sdk::{NetworkProxy, PluginError, PluginResult};

/// Retain the pinned SDK's proxy and redirect policy, adding platform trust to
/// Rustls without changing the TLS versions supported by provider requests.
pub fn http_client_builder(proxy: Option<&NetworkProxy>) -> PluginResult<reqwest::ClientBuilder> {
    configure_client(nora_provider_plugin_sdk::http_client_builder(proxy)?)
}

pub fn http_client(proxy: Option<&NetworkProxy>) -> PluginResult<reqwest::Client> {
    build(http_client_builder(proxy)?)
}

/// The pinned AI client's default transport uses reqwest's own proxy and
/// redirect policies. Keep those defaults for OAuthManager's standalone use.
pub fn default_http_client() -> PluginResult<reqwest::Client> {
    build(configure_client(reqwest::Client::builder())?)
}

fn configure_client(builder: reqwest::ClientBuilder) -> PluginResult<reqwest::ClientBuilder> {
    #[cfg(feature = "ipv4-fallback")]
    let builder = builder.ipv4_fallback(true);
    #[cfg(feature = "http2")]
    let builder = builder
        .http2_keep_alive_interval(std::time::Duration::from_secs(20))
        .http2_keep_alive_timeout(std::time::Duration::from_secs(10))
        .http2_keep_alive_while_idle(true);
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

#[cfg(all(test, feature = "http2"))]
#[path = "../test/http2_health.test.rs"]
mod http2_health_tests;

#[cfg(all(test, feature = "ipv4-fallback"))]
#[path = "../test/ipv4_fallback.test.rs"]
mod ipv4_fallback_tests;

#[cfg(all(test, feature = "ipv4-fallback"))]
#[path = "../test/ipv4_boundaries.test.rs"]
mod ipv4_boundary_tests;

#[cfg(test)]
#[allow(dead_code)]
#[path = "../vendor/reqwest/src/connect/ipv4_preference.rs"]
mod ipv4_preference_tests;
