extern crate self as nora_provider;

pub mod adapter;
pub mod auth;
pub mod definition;
pub mod discovery;
mod model_catalog;
pub mod models;
mod plugin;
pub mod transport;
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
#[path = "../../anthropic/src/provider.rs"]
pub(crate) mod anthropic_provider;
#[cfg(test)]
#[path = "../../github-copilot/src/provider.rs"]
pub(crate) mod copilot_provider;
#[cfg(test)]
#[path = "../../openai/src/provider.rs"]
pub(crate) mod openai_provider;
#[cfg(test)]
#[path = "../../xai/src/provider.rs"]
pub(crate) mod xai_provider;

#[cfg(test)]
pub(crate) fn test_adapter(id: &str) -> std::sync::Arc<dyn ProviderAdapter> {
    match id {
        "openai" | "openai-codex" => std::sync::Arc::new(openai_provider::OpenAI),
        "xai" => std::sync::Arc::new(xai_provider::Xai),
        "anthropic" => std::sync::Arc::new(anthropic_provider::Anthropic),
        "github-copilot" => std::sync::Arc::new(copilot_provider::Copilot),
        _ => adapter::generic(),
    }
}
