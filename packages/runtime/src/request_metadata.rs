use crate::definition::Definition;
use nora_provider_plugin_sdk::{FetchRequest, PluginError, PluginResult};
pub(crate) fn apply(definition: &Definition, request: &mut FetchRequest) -> PluginResult<()> {
    if definition.config.forwarded_headers.len() > 32 {
        return Err(PluginError::invalid(
            "The provider header forwarding declaration is invalid.",
        ));
    }
    let patterns = definition
        .config
        .forwarded_headers
        .iter()
        .map(|pattern| {
            globset::Glob::new(pattern)
                .map(|glob| glob.compile_matcher())
                .map_err(|_| {
                    PluginError::invalid("The provider header forwarding declaration is invalid.")
                })
        })
        .collect::<PluginResult<Vec<_>>>()?;
    for (key, value) in &request.metadata {
        let Some(name) = key.strip_prefix("header:") else {
            continue;
        };
        let lower = name.to_ascii_lowercase();
        if name.len() > 128
            || value.len() > 4096
            || value.chars().any(char::is_control)
            || lower.contains("authorization")
            || matches!(
                lower.as_str(),
                "x-api-key"
                    | "api-key"
                    | "cookie"
                    | "host"
                    | "x-oai-attestation"
                    | "x-openai-codex-luna-reserve"
                    | "x-codex-turn-state"
            )
            || !patterns.iter().any(|pattern| pattern.is_match(&lower))
        {
            continue;
        }
        if reqwest::header::HeaderName::from_bytes(lower.as_bytes()).is_err()
            || reqwest::header::HeaderValue::from_str(value).is_err()
        {
            continue;
        }
        request
            .headers
            .retain(|name, _| !name.eq_ignore_ascii_case(&lower));
        request.headers.insert(lower, value.clone());
    }
    Ok(())
}
#[cfg(test)]
#[path = "../test/request_metadata.test.rs"]
mod tests;
