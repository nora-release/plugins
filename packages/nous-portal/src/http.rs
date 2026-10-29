use futures::StreamExt;
use nora_ai::{HttpClient, HttpRequest, HttpStreamingResponse};
use nora_provider_plugin_sdk::{FetchResponse, PluginError, PluginResult};
use serde_json::Value;
use std::{collections::BTreeMap, time::Duration};

pub(crate) async fn send(
    client: &dyn HttpClient,
    request: HttpRequest,
    timeout: Duration,
) -> PluginResult<HttpStreamingResponse> {
    nora_provider_plugin_sdk::before_http_request().await?;
    unmetered(client, request, timeout).await
}

pub(crate) async fn unmetered(
    client: &dyn HttpClient,
    request: HttpRequest,
    timeout: Duration,
) -> PluginResult<HttpStreamingResponse> {
    tokio::time::timeout(timeout, client.stream(request))
        .await
        .map_err(|_| PluginError::transport("Nous Portal request timed out."))?
        .map_err(|_| PluginError::transport("Nous Portal could not be reached."))
}

pub(crate) async fn json(
    response: HttpStreamingResponse,
) -> PluginResult<(u16, BTreeMap<String, String>, Value)> {
    let status = response.status;
    let headers = response.headers;
    let bytes = tokio::time::timeout(
        Duration::from_secs(15),
        nora_ai::collect_http_body_text_limited(response.body, 1024 * 1024),
    )
    .await
    .map_err(|_| PluginError::transport("Nous Portal response timed out."))?
    .map_err(|_| {
        PluginError::transport("Nous Portal returned an incomplete or oversized response.")
    })?;
    Ok((
        status,
        headers,
        serde_json::from_str(&bytes).unwrap_or(Value::Null),
    ))
}

pub(crate) fn relay(response: HttpStreamingResponse) -> FetchResponse {
    FetchResponse {
        status: response.status,
        headers: response
            .headers
            .into_iter()
            .filter(|(name, _)| {
                !["content-length", "content-encoding"]
                    .contains(&name.to_ascii_lowercase().as_str())
            })
            .collect(),
        body: Box::pin(response.body.map(|chunk| {
            chunk
                .map(|bytes| bytes.to_vec())
                .map_err(|_| PluginError::transport("Nous Portal response stopped."))
        })),
    }
}

pub(crate) fn caller_headers(headers: BTreeMap<String, String>) -> BTreeMap<String, String> {
    headers
        .into_iter()
        .filter(|(name, value)| {
            let name = name.to_ascii_lowercase();
            ![
                "authorization",
                "x-api-key",
                "x-nous-refresh-token",
                "cookie",
                "proxy-authorization",
                "content-length",
                "host",
                "connection",
                "transfer-encoding",
                "upgrade",
                "te",
                "trailer",
                "proxy-connection",
                "keep-alive",
            ]
            .contains(&name.as_str())
                && !name.starts_with("x-magpie-")
                && !name.starts_with("x-nora-")
                && name.len() <= 128
                && value.len() <= 16384
                && !value.chars().any(char::is_control)
        })
        .collect()
}

#[cfg(test)]
#[path = "../test/http.test.rs"]
mod tests;
