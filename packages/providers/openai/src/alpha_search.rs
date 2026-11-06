use nora_ai::{HttpClient, HttpMethod, HttpRequest, ResolvedProviderAuth};
use nora_provider::usage::header;
use nora_provider_plugin_sdk::{
    PluginError, PluginResult, MAX_REQUEST_BYTES, MAX_RESPONSE_FRAME_BYTES,
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, time::Duration};

const ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/alpha/search";
const OUTPUT_LIMIT: usize = MAX_RESPONSE_FRAME_BYTES - 1024;

fn codex_header(name: &str) -> bool {
    if name.contains("authorization")
        || name.contains("api-key")
        || name.contains("apikey")
        || [
            "x-oai-attestation",
            "x-openai-codex-luna-reserve",
            "x-codex-turn-state",
        ]
        .contains(&name)
    {
        return false;
    }
    [
        "session-id",
        "thread-id",
        "session_id",
        "conversation_id",
        "x-client-request-id",
        "version",
    ]
    .contains(&name)
        || name.starts_with("x-openai-")
        || name.starts_with("x-codex-")
}
fn failed() -> PluginError {
    PluginError::transport("ChatGPT's native search could not complete.")
}

pub(crate) async fn search(
    auth: &ResolvedProviderAuth,
    body: Value,
    headers: BTreeMap<String, String>,
    client: &dyn HttpClient,
) -> PluginResult<Value> {
    if auth.credential_provider_id.as_deref() != Some("openai-codex") {
        return Err(PluginError::unsupported("native search for this account"));
    }
    let token = auth
        .token
        .as_deref()
        .filter(|value| !value.is_empty() && !value.chars().any(char::is_control))
        .ok_or_else(|| PluginError::authentication("Connect a ChatGPT account."))?;
    let account = header(auth, "chatgpt-account-id")
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 512
                && value.trim() == *value
                && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| {
            PluginError::authentication("This ChatGPT account has no account ID. Sign in again.")
        })?;
    if !body.is_object() {
        return Err(PluginError::invalid(
            "Provide a native search request object.",
        ));
    }
    let bytes = serde_json::to_vec(&body)
        .map_err(|_| PluginError::invalid("The native search request is invalid."))?;
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(PluginError::invalid(
            "The native search request exceeds its size limit.",
        ));
    }
    let mut request = HttpRequest::new(HttpMethod::Post, ENDPOINT).with_body(bytes);
    for (name, value) in headers {
        let name = name.to_ascii_lowercase();
        if codex_header(&name)
            && name.len() <= 128
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            && value.len() <= 16_384
            && !value.chars().any(char::is_control)
        {
            request.headers.insert(name, value);
        }
    }
    let version = crate::provider::codex_version::newest([
        Some(crate::provider::codex_version::MINIMUM),
        header(auth, "version"),
        request.headers.get("version").map(String::as_str),
    ]);
    request
        .headers
        .extend(crate::provider::codex_version::headers(&version));
    request.headers.extend(BTreeMap::from([
        ("authorization".into(), format!("Bearer {token}")),
        ("chatgpt-account-id".into(), account.into()),
        ("content-type".into(), "application/json".into()),
        ("accept".into(), "application/json".into()),
        ("openai-beta".into(), "responses=experimental".into()),
    ]));
    let operation = async {
        let response = client.stream(request).await.map_err(|_| failed())?;
        let status = response.status;
        let retry_after_ms =
            nora_ai::ProviderFailure::http_response(status, "", &response.headers).retry_after_ms;
        let retry_after = response
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
            .map(|(_, value)| value)
            .filter(|value| value.len() <= 256 && !value.chars().any(char::is_control))
            .cloned();
        let mut stream = response.body;
        let limit = if (200..300).contains(&status) {
            OUTPUT_LIMIT
        } else {
            4096
        };
        let mut bytes = Vec::new();
        while let Some(chunk) = std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await {
            let chunk = chunk.map_err(|_| failed())?;
            if bytes.len().saturating_add(chunk.len()) > limit {
                if (200..300).contains(&status) {
                    return Err(failed());
                }
                bytes.extend_from_slice(&chunk[..limit.saturating_sub(bytes.len())]);
                break;
            }
            bytes.extend_from_slice(&chunk);
        }
        let body: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) if !(200..300).contains(&status) => {
                Value::String(String::from_utf8_lossy(&bytes).into_owned())
            }
            Err(_) => return Err(failed()),
        };
        if (200..300).contains(&status) && !body.is_object() {
            return Err(failed());
        }
        let mut result = json!({"status":status,"body":body});
        if !(200..300).contains(&status) {
            if let Some(delay) = retry_after_ms {
                result["retryAfterMs"] = json!(delay);
            }
            if let Some(value) = retry_after {
                result["retryAfter"] = json!(value);
            }
        }
        if serde_json::to_vec(&result).map_or(true, |bytes| bytes.len() > OUTPUT_LIMIT) {
            return Err(failed());
        }
        Ok(result)
    };
    tokio::time::timeout(Duration::from_secs(120), operation)
        .await
        .map_err(|_| failed())?
}

#[cfg(test)]
#[path = "../test/alpha_search.test.rs"]
mod tests;
