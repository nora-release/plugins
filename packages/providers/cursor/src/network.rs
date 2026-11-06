use futures::StreamExt;
use nora_ai::{HttpClient, HttpMethod, HttpRequest, HttpStreamingResponse};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};

pub(crate) const API: &str = "https://api2.cursor.sh";
pub(crate) const BASE: &str = "https://api2.cursor.sh/nora/v1";
pub(crate) const FALLBACK_VERSION: &str = "cli-2026.09.23-86fc751";
pub(crate) fn headers(request: HttpRequest, token: &str, version: &str) -> HttpRequest {
    let request = if token.is_empty() {
        request
    } else {
        request.with_header("authorization", format!("Bearer {token}"))
    };
    request
        .with_header("x-cursor-client-version", version)
        .with_header("x-cursor-client-type", "cli")
        .with_header("x-ghost-mode", "true")
}
pub(crate) fn post(
    url: impl Into<String>,
    body: Vec<u8>,
    token: &str,
    version: &str,
    content: &str,
) -> HttpRequest {
    let mut request = headers(
        HttpRequest::new(HttpMethod::Post, url.into()),
        token,
        version,
    )
    .with_header("content-type", content)
    .with_header("connect-protocol-version", "1");
    request.body = Some(body);
    request
}
pub(crate) fn failure(status: u16, value: &Value) -> PluginError {
    let value = value
        .get("error")
        .filter(|v| v.is_object())
        .unwrap_or(value);
    let code = value["code"].as_str().unwrap_or_default();
    let message_hint = value["message"]
        .as_str()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let details_hint = value["details"].to_string().to_ascii_lowercase();
    if (status == 403 || code == "permission_denied")
        && (message_hint.contains("region") || details_hint.contains("region"))
    {
        return PluginError::new(
            "cursor_region_required",
            "Cursor serves this account through its regional agent endpoint. (HTTP 403)",
        );
    }
    if code == "max_mode_required"
        || message_hint.contains("max mode required")
        || message_hint.contains("max_mode_required")
        || message_hint.contains("requires max mode")
        || details_hint.contains("max_mode_required")
        || details_hint.contains("max mode required")
    {
        return PluginError::new(
            "cursor_max_mode_required",
            "Cursor requires Max Mode for this model.",
        );
    }
    // Server bodies may echo credentials, tool inputs or prompts. Only stable status/code is public.
    let message = if status == 401 || code == "unauthenticated" {
        "Cursor sign-in expired. Reconnect this account."
    } else if status == 429 || code == "resource_exhausted" {
        "Cursor usage limit reached."
    } else if status == 403 || code == "permission_denied" {
        "Cursor refused this account or its region."
    } else if status == 400 || code == "invalid_argument" {
        "Cursor rejected the model request or its context length."
    } else {
        "Cursor could not complete the request."
    };
    let status = match code {
        "unauthenticated" => 401,
        "permission_denied" => 403,
        "resource_exhausted" => 429,
        "invalid_argument" => 400,
        "unavailable" => 503,
        _ => status,
    };
    PluginError::new(
        if status == 401 {
            "authentication_required".into()
        } else {
            format!("cursor_http_{status}")
        },
        format!("{message} (HTTP {status})"),
    )
}
pub(crate) async fn send(
    client: &dyn HttpClient,
    request: HttpRequest,
    seconds: u64,
) -> PluginResult<HttpStreamingResponse> {
    tokio::time::timeout(Duration::from_secs(seconds), client.stream(request))
        .await
        .map_err(|_| PluginError::transport("The Cursor request timed out."))?
        .map_err(|_| PluginError::transport("The Cursor service could not be reached."))
}
pub(crate) async fn read(response: HttpStreamingResponse) -> PluginResult<Value> {
    let status = response.status;
    let text = tokio::time::timeout(Duration::from_secs(30), async move {
        let mut body = response.body;
        let mut bytes = Vec::new();
        while let Some(chunk) = body.next().await {
            let chunk =
                chunk.map_err(|_| PluginError::transport("The Cursor response is incomplete."))?;
            if bytes.len().saturating_add(chunk.len()) > 8 * 1024 * 1024 {
                return Err(PluginError::transport("The Cursor response is too large."));
            }
            bytes.extend_from_slice(&chunk);
        }
        String::from_utf8(bytes)
            .map_err(|_| PluginError::transport("The Cursor response is not valid UTF-8."))
    })
    .await
    .map_err(|_| PluginError::transport("The Cursor response timed out."))??;
    let value = serde_json::from_str(&text);
    if !(200..300).contains(&status) {
        return Err(failure(status, &value.unwrap_or(Value::Null)));
    }
    value.map_err(|_| PluginError::transport("The Cursor response is invalid JSON."))
}
pub(crate) async fn unary(
    client: &dyn HttpClient,
    path: &str,
    token: &str,
    version: &str,
    body: &Value,
) -> PluginResult<Value> {
    let request = post(
        format!("{API}/{path}"),
        serde_json::to_vec(body)
            .map_err(|_| PluginError::invalid("The Cursor request is invalid."))?,
        token,
        version,
        "application/json",
    );
    tokio::time::timeout(Duration::from_secs(30), async {
        read(send(client, request, 30).await?).await
    })
    .await
    .map_err(|_| PluginError::transport("The Cursor request timed out."))?
}
pub(crate) async fn identify(
    client: &dyn HttpClient,
    token: &str,
    version: &str,
) -> PluginResult<(String, String)> {
    let me = unary(
        client,
        "aiserver.v1.DashboardService/GetMe",
        token,
        version,
        &json!({}),
    )
    .await?;
    let email = me["email"]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= 512 && !s.chars().any(char::is_control))
        .ok_or_else(|| PluginError::authentication("Cursor did not identify this account."))?
        .trim()
        .to_owned();
    let plan = unary(
        client,
        "aiserver.v1.DashboardService/GetPlanInfo",
        token,
        version,
        &json!({}),
    )
    .await
    .ok()
    .and_then(|v| {
        v.pointer("/planInfo/planName")
            .and_then(Value::as_str)
            .map(str::to_owned)
    })
    .unwrap_or_default();
    Ok((email, plan))
}
pub(crate) async fn agent_url(client: &dyn HttpClient, token: &str, version: &str) -> String {
    let config = unary(
        client,
        "aiserver.v1.ServerConfigService/GetServerConfig",
        token,
        version,
        &json!({}),
    )
    .await
    .ok();
    for path in ["/agentUrlConfig/agentUrl", "/agentUrlConfig/agentnUrl"] {
        if let Some(url) = config
            .as_ref()
            .and_then(|c| c.pointer(path))
            .and_then(Value::as_str)
            .and_then(|s| url::Url::parse(s).ok())
        {
            let host = url.host_str().unwrap_or_default();
            if url.scheme() == "https"
                && (host.ends_with(".cursor.sh") || host.ends_with(".cursor.com"))
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
            {
                return url.as_str().trim_end_matches('/').to_owned();
            }
        }
    }
    "https://agentn.global.api5.cursor.sh".into()
}
pub(crate) type Client = Arc<dyn HttpClient>;
