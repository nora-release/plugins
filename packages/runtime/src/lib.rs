mod pricing;
pub mod profiles;
mod request_metadata;
extern crate self as nora_provider;

mod account_headers;
pub mod account_import;
pub mod adapter;
pub mod auth;
mod auth_headers;
mod balance_path;
mod balance_sources;
mod build_capabilities;
pub mod catalog;
mod connection_patch;
pub mod definition;
pub mod discovery;
mod endpoint_overrides;
mod extra_body;
mod http_response;
pub mod media_upload;
mod model_catalog;
mod model_metadata;
pub mod models;
mod network_client;
mod plugin;
mod tls_verifier;
pub mod transport;
mod transport_file;
pub mod usage;

pub use adapter::{GenericProvider, ProviderAdapter};
pub use plugin::BundledProvider;

#[cfg(test)]
#[path = "../test/tls_support.rs"]
mod tls_support;

#[cfg(test)]
#[path = "../test/compatible.test.rs"]
mod compatible_tests;

pub async fn run(adapter: impl ProviderAdapter) {
    if let Err(error) = nora_provider_plugin_sdk::serve(BundledProvider::new(adapter)).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
#[path = "../test/fixtures.rs"]
mod fixtures;
#[cfg(test)]
pub(crate) use fixtures::{
    anthropic_provider, copilot_provider, openai_provider, test_adapter, xai_provider,
};
