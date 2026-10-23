mod pricing;
pub mod profiles;
mod request_metadata;
extern crate self as nora_provider;

mod account_headers;
pub mod adapter;
pub mod auth;
mod auth_headers;
pub mod catalog;
pub mod definition;
pub mod discovery;
pub mod media_upload;
mod model_catalog;
pub mod models;
mod plugin;
pub mod transport;
mod transport_file;
pub mod usage;

pub use adapter::{GenericProvider, ProviderAdapter};
pub use plugin::BundledProvider;

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
