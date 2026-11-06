use nora_ai::{HttpClient, HttpMethod, HttpRequest, HttpStreamingResponse};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use nora_provider_subscription_core as core;
use serde_json::Value;
use std::{sync::Arc, time::Duration};
pub(crate) type Client = Arc<dyn HttpClient>;
pub(crate) const API: &str = "https://api.factory.ai";
pub(crate) const EU: &str = "https://api.eu.factory.ai";
pub(crate) const BASE: &str = "https://api.factory.ai/api/llm/a/v1";
pub(crate) const WORKOS: &str = "https://api.workos.com/user_management";
pub(crate) const CLIENT_ID: &str = "client_01HNM792M5G5G1A2THWPXKFMXB";
pub(crate) const VERSION: &str = "0.242.0";
pub(crate) fn error(status: u16) -> PluginError {
    PluginError::new(format!("factory_http_{status}"),match status{400=>"Factory rejected this request.",401=>"Factory refused this account.",403=>"Factory refused this organization's request.",413=>"Factory's request body byte limit was exceeded. Reduce attachments or compact the conversation.",429=>"Factory rate or usage limit reached.",_=>"Factory could not complete the request."})
}
pub(crate) async fn send(
    client: &dyn HttpClient,
    request: HttpRequest,
) -> PluginResult<HttpStreamingResponse> {
    tokio::time::timeout(Duration::from_secs(60), client.stream(request))
        .await
        .map_err(|_| PluginError::transport("The Factory request timed out."))?
        .map_err(|_| PluginError::transport("The Factory service could not be reached."))
}
pub(crate) async fn read(response: HttpStreamingResponse) -> PluginResult<(u16, Value)> {
    let status = response.status;
    let bytes = core::collect(response, core::MAX_JSON).await?;
    let value = serde_json::from_slice(&bytes);
    if !(200..300).contains(&status) {
        return Ok((
            status,
            value.unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned())),
        ));
    }
    Ok((
        status,
        value.map_err(|_| PluginError::transport("Factory returned invalid JSON."))?,
    ))
}
pub(crate) async fn checked(client: &dyn HttpClient, request: HttpRequest) -> PluginResult<Value> {
    let (status, value) = read(send(client, request).await?).await?;
    if !(200..300).contains(&status) {
        return Err(error(status));
    }
    Ok(value)
}
pub(crate) fn factory(
    method: HttpMethod,
    url: String,
    account: &super::account::Account,
) -> HttpRequest {
    let request = HttpRequest::new(method, url)
        .with_header("authorization", format!("Bearer {}", account.access))
        .with_header("x-factory-client", "cli")
        .with_header("x-client-version", VERSION)
        .with_header("user-agent", format!("factory-cli/{VERSION}"));
    if !account.key && !account.active_org.is_empty() {
        request.with_header("x-factory-org-id", &account.active_org)
    } else {
        request
    }
}
pub(crate) async fn whoami(
    client: &dyn HttpClient,
    account: &super::account::Account,
) -> PluginResult<Value> {
    let mut request = HttpRequest::new(
        HttpMethod::Get,
        format!("{}/api/cli/whoami", account.base()),
    )
    .with_header("authorization", format!("Bearer {}", account.access))
    .with_header("x-factory-whoami-extended", "true");
    if !account.key && !account.active_org.is_empty() {
        request = request.with_header("x-factory-org-id", &account.active_org);
    }
    checked(client, request).await
}
pub(crate) async fn reconcile(
    client: &dyn HttpClient,
    account: &mut super::account::Account,
) -> PluginResult<bool> {
    let who = match whoami(client, account).await {
        Ok(who) => who,
        Err(error) if error.code == "factory_http_403" && !account.active_org.is_empty() => {
            let mut unscoped = account.clone();
            unscoped.active_org.clear();
            whoami(client, &unscoped).await?
        }
        Err(error) => return Err(error),
    };
    account.apply_identity(&who)
}
pub(crate) fn content_refused(
    status: u16,
    value: &Value,
    headers: &std::collections::BTreeMap<String, String>,
) -> bool {
    status == 403
        && !headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("x-factory-upstream-error"))
        && (value["code"] == "content_policy_rejected"
            || value["error"]["code"] == "content_policy_rejected")
}
pub(crate) fn org_refused(status: u16, value: &Value) -> bool {
    status == 403
        && value
            .to_string()
            .to_ascii_lowercase()
            .contains("active organization is not accessible")
}
