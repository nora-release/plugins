use super::protobuf::Pb;
use nora_ai::{HttpClient, HttpMethod, HttpRequest, HttpStreamingResponse};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
pub(crate) type Client = Arc<dyn HttpClient>;
pub(crate) const SERVER: &str = "https://server.codeium.com";
pub(crate) const BASE: &str = "https://server.codeium.com/devin/v1";
pub(crate) const VERSION: &str = "3000.11.3";
pub(crate) const MODELS: &str = "/exa.api_server_pb.ApiServerService/GetCliModelConfigs";
pub(crate) const CHAT: &str = "/exa.api_server_pb.ApiServerService/GetChatMessage";
pub(crate) fn os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "windows",
        other => other,
    }
}
pub(crate) fn metadata(key: &str) -> Pb {
    Pb::new()
        .text(1, "devin-cli")
        .text(2, VERSION)
        .text(3, key)
        .text(4, "en")
        .text(5, os())
        .text(7, VERSION)
        .text(12, "chisel")
        .text(28, "chisel")
}
pub(crate) fn status(status: u16, value: &Value) -> u16 {
    let value = value.get("error").unwrap_or(value);
    let code = value["code"].as_str().unwrap_or_default();
    let message = value["message"]
        .as_str()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match code {
        "unauthenticated" => 401,
        "resource_exhausted" => 429,
        "invalid_argument" => 400,
        "unavailable" => 503,
        _ if message.contains("quota") || message.contains("rate limit") => 429,
        _ if message.contains("content policy")
            || message.contains("too long")
            || message.contains("context length")
            || message.contains("too many tokens") =>
        {
            400
        }
        _ if !code.is_empty() => 502,
        _ => status,
    }
}
pub(crate) fn error(status: u16) -> PluginError {
    PluginError::new(if status==401{"authentication_required".into()}else{format!("devin_http_{status}")},match status{400=>"Devin refused this request or its context.",401=>"Devin sign-in expired. Reconnect this account.",403=>"This Devin account cannot make this request.",413=>"The Devin request body is too large. Reduce attachments or compact the conversation.",415=>"This Devin model or native protocol cannot accept these images or files.",429=>"The Devin account's usage or rate limit was reached.",_=>"Devin could not complete the request."})
}
pub(crate) async fn send(
    client: &dyn HttpClient,
    request: HttpRequest,
) -> PluginResult<HttpStreamingResponse> {
    tokio::time::timeout(Duration::from_secs(30), client.stream(request))
        .await
        .map_err(|_| PluginError::transport("The Devin request timed out."))?
        .map_err(|_| PluginError::transport("The Devin service could not be reached."))
}
pub(crate) async fn collect(
    response: HttpStreamingResponse,
    limit: usize,
) -> PluginResult<Vec<u8>> {
    tokio::time::timeout(
        Duration::from_secs(30),
        nora_provider_subscription_core::collect(response, limit),
    )
    .await
    .map_err(|_| PluginError::transport("The Devin response timed out."))?
}
pub(crate) async fn json(client: &dyn HttpClient, request: HttpRequest) -> PluginResult<Value> {
    let response = send(client, request).await?;
    let http = response.status;
    let body = collect(response, 8 * 1024 * 1024).await?;
    let value = serde_json::from_slice::<Value>(&body);
    if !(200..300).contains(&http) {
        return Err(error(status(http, value.as_ref().unwrap_or(&Value::Null))));
    }
    let value = value.map_err(|_| PluginError::transport("Devin returned invalid JSON."))?;
    let http = status(http, &value);
    if !(200..300).contains(&http) {
        return Err(error(http));
    }
    Ok(value)
}
pub(crate) async fn user(client: &dyn HttpClient, key: &str, server: &str) -> PluginResult<Value> {
    let request=HttpRequest::new(HttpMethod::Post,format!("{server}/exa.seat_management_pb.SeatManagementService/GetUserStatus")).with_header("content-type","application/json").with_header("connect-protocol-version","1").with_body(json!({"metadata":{"ideName":"devin-cli","ideVersion":VERSION,"extensionName":"devin-cli","extensionVersion":VERSION,"apiKey":key,"locale":"en","os":os()}}).to_string().into_bytes());
    json(client, request).await
}
