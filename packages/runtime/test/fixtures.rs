use super::{adapter, ProviderAdapter};

#[path = "../../anthropic/src/provider.rs"]
pub(crate) mod anthropic_provider;
#[path = "../../github-copilot/src/provider.rs"]
pub(crate) mod copilot_provider;
#[path = "../../openai/src/provider.rs"]
pub(crate) mod openai_provider;
#[path = "../../xai/src/provider.rs"]
pub(crate) mod xai_provider;

pub(crate) fn test_adapter(id: &str) -> std::sync::Arc<dyn ProviderAdapter> {
    match id {
        "openai" | "openai-codex" => std::sync::Arc::new(openai_provider::OpenAI),
        "xai" => std::sync::Arc::new(xai_provider::Xai::default()),
        "anthropic" => std::sync::Arc::new(anthropic_provider::Anthropic),
        "github-copilot" => std::sync::Arc::new(copilot_provider::Copilot::default()),
        _ => adapter::generic(),
    }
}
