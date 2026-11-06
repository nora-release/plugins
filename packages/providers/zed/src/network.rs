use futures::StreamExt;
use nora_ai::{HttpClient, HttpMethod, HttpRequest, HttpStreamingResponse};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
pub(crate) type Client = Arc<dyn HttpClient>;
pub(crate) const CLOUD: &str = "https://cloud.zed.dev";
pub(crate) const BASE: &str = "https://cloud.zed.dev/v1";
pub(crate) const VERSION: &str = "1.23.0";
pub(crate) fn ua() -> String {
    format!(
        "Zed/{VERSION} ({}; {})",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}
pub(crate) fn request(method: HttpMethod, path: &str) -> HttpRequest {
    HttpRequest::new(method, format!("{CLOUD}{path}"))
        .with_header("user-agent", ua())
        .with_header("content-type", "application/json")
}
pub(crate) fn failure(status: u16) -> PluginError {
    PluginError::new(
        if status == 401 {
            "authentication_required".into()
        } else {
            format!("zed_http_{status}")
        },
        match status {
            401 => "Zed sign-in expired. Reconnect this account.",
            402 => "This Zed organization has no available subscription allowance.",
            403 => "This Zed organization does not permit these models.",
            429 => "Zed rate or usage limit reached.",
            _ => "Zed could not complete the request.",
        },
    )
}
pub(crate) async fn send(
    client: &dyn HttpClient,
    request: HttpRequest,
) -> PluginResult<HttpStreamingResponse> {
    tokio::time::timeout(Duration::from_secs(30), client.stream(request))
        .await
        .map_err(|_| PluginError::transport("The Zed request timed out."))?
        .map_err(|_| PluginError::transport("The Zed service could not be reached."))
}
pub(crate) async fn read(response: HttpStreamingResponse) -> PluginResult<Value> {
    let status = response.status;
    let bytes = tokio::time::timeout(Duration::from_secs(30), async move {
        let mut stream = response.body;
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|_| PluginError::transport("The Zed response is incomplete."))?;
            if bytes.len().saturating_add(chunk.len()) > 8 * 1024 * 1024 {
                return Err(PluginError::transport("The Zed response is too large."));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    })
    .await
    .map_err(|_| PluginError::transport("The Zed response timed out."))??;
    if !(200..300).contains(&status) {
        return Err(failure(status));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| PluginError::transport("The Zed response is invalid JSON."))
}
pub(crate) fn header<'a>(response: &'a HttpStreamingResponse, name: &str) -> Option<&'a str> {
    response
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}
pub(crate) fn stale(response: &HttpStreamingResponse) -> bool {
    response.status == 401
        || header(response, "x-zed-expired-token").is_some()
        || header(response, "x-zed-outdated-token").is_some()
}
