use nora_provider_plugin_sdk::{PluginError, PluginResult};
use std::collections::BTreeMap;
use url::Url;

/// Codex sends text turns compressed to ChatGPT. Run after all body edits so
/// native history and account-specific options are encoded exactly once.
pub(super) fn finish(
    target: &Url,
    headers: &mut BTreeMap<String, String>,
    body: &mut Option<Vec<u8>>,
) -> PluginResult<()> {
    if target.scheme() != "https"
        || target.host_str() != Some("chatgpt.com")
        || target.path() != "/backend-api/codex/responses"
    {
        return Ok(());
    }
    if headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("content-encoding")
            && !value.is_empty()
            && !value.eq_ignore_ascii_case("identity")
    }) {
        return Ok(());
    }
    let Some(bytes) = body.as_deref() else {
        return Ok(());
    };
    let encoded = zstd::stream::encode_all(bytes, 1)
        .map_err(|_| PluginError::transport("The Codex request could not be compressed."))?;
    headers.retain(|name, _| {
        !name.eq_ignore_ascii_case("content-length")
            && !name.eq_ignore_ascii_case("content-encoding")
    });
    headers.insert("content-encoding".into(), "zstd".into());
    *body = Some(encoded);
    Ok(())
}
