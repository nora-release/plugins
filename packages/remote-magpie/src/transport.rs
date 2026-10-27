use crate::config::Account;
use futures::StreamExt;
use nora_ai::{HttpClient, HttpClientError, HttpMethod, HttpRequest, HttpStreamingResponse};
use nora_provider_plugin_sdk::{FetchRequest, FetchResponse, PluginError, PluginResult};
use serde_json::{json, Value};
use std::{collections::BTreeMap, time::Duration};

#[derive(Clone, Debug)]
pub(crate) struct RemoteError {
    pub status: u16,
    pub held: bool,
    pub message: String,
}
impl RemoteError {
    pub fn plugin(self) -> PluginError {
        if self.status == 401 {
            PluginError::authentication(self.message)
        } else {
            PluginError::transport(self.message)
        }
    }
}
fn header<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}
fn clean(value: &str) -> bool {
    value.len() <= 4096 && !value.chars().any(char::is_control)
}
fn private(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("authorization")
        || name.contains("api-key")
        || matches!(
            name.as_str(),
            "cookie"
                | "set-cookie"
                | "host"
                | "content-length"
                | "connection"
                | "transfer-encoding"
                | "x-oai-attestation"
                | "x-openai-codex-luna-reserve"
                | "x-codex-turn-state"
        )
        || name.starts_with("x-nora-")
}
fn caller_header(name: &str) -> bool {
    if private(name) {
        return false;
    }
    matches!(
        name,
        "user-agent"
            | "originator"
            | "x-app"
            | "session-id"
            | "thread-id"
            | "session_id"
            | "conversation_id"
            | "x-conversation-id"
            | "x-client-request-id"
            | "version"
            | "x-magpie-agent"
            | "x-magpie-via"
            | "x-magpie-session"
    ) || name.starts_with("x-openai-")
        || name.starts_with("x-codex-")
}
pub(crate) fn request_headers(
    account: &Account,
    request: &FetchRequest,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, value) in &request.headers {
        let lower = name.to_ascii_lowercase();
        if !private(&lower)
            && clean(value)
            && matches!(
                lower.as_str(),
                "content-type"
                    | "accept"
                    | "range"
                    | "if-range"
                    | "accept-encoding"
                    | "anthropic-version"
                    | "anthropic-beta"
                    | "openai-beta"
                    | "user-agent"
                    | "originator"
                    | "x-app"
            )
        {
            out.insert(lower, value.clone());
        }
    }
    for (key, value) in &request.metadata {
        let Some(name) = key.strip_prefix("header:") else {
            continue;
        };
        let name = name.to_ascii_lowercase();
        if caller_header(&name)
            && clean(value)
            && reqwest::header::HeaderName::from_bytes(name.as_bytes()).is_ok()
        {
            out.insert(name, value.clone());
        }
    }
    for (key, name) in [
        ("agent", "x-magpie-agent"),
        ("via", "x-magpie-via"),
        ("sessionID", "x-magpie-session"),
    ] {
        if let Some(value) = request
            .metadata
            .get(key)
            .filter(|value| clean(value) && !value.is_empty())
        {
            out.insert(name.into(), value.clone());
        }
    }
    out.entry("user-agent".into())
        .or_insert_with(|| "nora/1.1.0".into());
    if request
        .metadata
        .get("noraSearching")
        .is_some_and(|value| value == "1")
    {
        out.insert("x-magpie-searching".into(), "1".into());
        out.insert("user-agent".into(), "magpie/nora".into());
    }
    out.insert("accept-encoding".into(), "identity".into());
    out.insert("authorization".into(), format!("Bearer {}", account.key));
    out
}
pub(crate) async fn read(
    client: &dyn HttpClient,
    account: &Account,
    method: HttpMethod,
    path: &str,
    models: bool,
) -> Result<Value, RemoteError> {
    let mut headers = BTreeMap::from([
        ("authorization".into(), format!("Bearer {}", account.key)),
        ("accept".into(), "application/json".into()),
        ("accept-encoding".into(), "identity".into()),
        ("user-agent".into(), "nora/1.1.0".into()),
    ]);
    if models {
        for name in [
            "X-Magpie-Drawers",
            "X-Magpie-Videomakers",
            "X-Magpie-Prices",
            "X-Magpie-Deciders",
        ] {
            headers.insert(name.into(), "1".into());
        }
    }
    let request = HttpRequest {
        method,
        url: account.at(path),
        headers,
        body: None,
    };
    let result = tokio::time::timeout(Duration::from_secs(if models { 20 } else { 8 }), async {
        let response = client
            .stream(request)
            .await
            .map_err(|_| failure(0, "Could not reach the remote gateway.", false))?;
        let status = response.status;
        let bytes = collect(
            response,
            if models || path.contains("quotas/") {
                8 * 1024 * 1024
            } else {
                1024 * 1024
            },
        )
        .await
        .map_err(|_| {
            failure(
                status,
                "The remote gateway response was incomplete or too large.",
                false,
            )
        })?;
        if !(200..300).contains(&status) {
            return Err(response_error(
                account,
                status,
                &serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            ));
        }
        serde_json::from_slice(&bytes).map_err(|_| {
            failure(
                status,
                "The remote gateway returned an unreadable response.",
                false,
            )
        })
    })
    .await;
    result.unwrap_or_else(|_| {
        Err(failure(
            0,
            "The remote gateway did not answer in time.",
            false,
        ))
    })
}
fn failure(status: u16, message: &str, held: bool) -> RemoteError {
    RemoteError {
        status,
        held,
        message: message.into(),
    }
}
fn response_error(account: &Account, status: u16, value: &Value) -> RemoteError {
    let held = status == 403
        && value.pointer("/error/code").and_then(Value::as_str) == Some("gateway_key_held");
    let message = match status {
        401 => "The remote gateway did not accept this API key. Use an enabled gateway key.".into(),
        403 if held => {
            "This gateway key can read its own limits but cannot read the remote accounts.".into()
        }
        403 => "This gateway key cannot read the requested remote information.".into(),
        404 | 405 => {
            "The remote gateway does not support this information. Update it on that computer."
                .into()
        }
        _ => value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .filter(|value| value.len() <= 4096)
            .map(|value| value.replace(&account.key, "[redacted]"))
            .unwrap_or_else(|| format!("The remote gateway returned HTTP {status}.")),
    };
    RemoteError {
        status,
        held,
        message,
    }
}
pub(crate) async fn collect(
    mut response: HttpStreamingResponse,
    maximum: usize,
) -> PluginResult<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(chunk) = response.body.next().await {
        let chunk =
            chunk.map_err(|_| PluginError::transport("The remote gateway response stopped."))?;
        if chunk.len() > maximum.saturating_sub(out.len()) {
            return Err(PluginError::transport(
                "The remote gateway response is too large.",
            ));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}
pub(crate) fn response_headers(
    status: u16,
    headers: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, value) in headers {
        let name = name.to_ascii_lowercase();
        if clean(value)
            && matches!(
                name.as_str(),
                "content-type"
                    | "content-length"
                    | "content-disposition"
                    | "content-range"
                    | "accept-ranges"
                    | "etag"
                    | "last-modified"
                    | "cache-control"
                    | "retry-after"
                    | "retry-after-ms"
                    | "x-request-id"
                    | "x-magpie-limit-reset"
            )
        {
            out.insert(name, value.clone());
        }
    }
    // A remote gateway serves many independent suppliers. Its ordinary refusals
    // belong to this model; only its key's own marked budget rests the key.
    let key_reset = header(headers, "x-magpie-limit-reset").filter(|value| !value.is_empty());
    let account = status == 401 || status == 429 && key_reset.is_some();
    out.insert(
        "X-Nora-Failure-Scope".into(),
        if account { "account" } else { "model" }.into(),
    );
    if status == 429 {
        if let Some(reset) = key_reset {
            out.insert("X-Nora-Client-Limit".into(), "true".into());
            if chrono::DateTime::parse_from_rfc3339(reset).is_ok() {
                out.insert("X-Nora-Reset-At".into(), reset.into());
            }
        }
    }
    out
}
pub(crate) async fn fetch(
    client: &dyn HttpClient,
    account: &Account,
    request: FetchRequest,
) -> PluginResult<FetchResponse> {
    account.target(&request.url)?;
    let method = match request.method.as_str() {
        "POST" => HttpMethod::Post,
        "GET" => HttpMethod::Get,
        _ => return Err(PluginError::invalid("Unsupported remote request method.")),
    };
    let headers = request_headers(account, &request);
    let response = if let Some(file) = &request.body_file {
        if request.body.is_some() {
            return Err(PluginError::invalid("Supply only one request body."));
        }
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|_| PluginError::invalid("Invalid remote request method."))?;
        let raw = nora_provider_network::http_client(request.network_proxy.as_ref())?;
        let mut outgoing = raw.request(method, &request.url);
        for (name, value) in &headers {
            outgoing = outgoing.header(name, value);
        }
        let response = nora_provider_plugin_sdk::send_http(
            outgoing
                .header("content-length", file.length)
                .body(file.http_body().await?),
        )
        .await?;
        HttpStreamingResponse {
            status: response.status().as_u16(),
            headers: response
                .headers()
                .iter()
                .filter_map(|(name, value)| {
                    value
                        .to_str()
                        .ok()
                        .map(|value| (name.to_string(), value.to_owned()))
                })
                .collect(),
            body: Box::pin(response.bytes_stream().map(|chunk| {
                chunk.map_err(|_| {
                    HttpClientError::Transport("The remote gateway response stopped.".into())
                })
            })),
        }
    } else {
        client
            .stream(HttpRequest {
                method,
                url: request.url.clone(),
                headers,
                body: request.decode_body()?,
            })
            .await
            .map_err(|_| PluginError::transport("Could not reach the remote gateway."))?
    };
    let status = response.status;
    let mut headers = response_headers(status, &response.headers);
    if status >= 400 {
        let body=collect(response,1024*1024).await.map(|bytes|String::from_utf8_lossy(&bytes).replace(&account.key,"[redacted]").into_bytes()).unwrap_or_else(|_|json!({"error":{"message":"The remote gateway error response was incomplete.","type":"upstream_error"}}).to_string().into_bytes());
        headers.remove("content-length");
        return Ok(FetchResponse::bytes(status, headers, body));
    }
    Ok(FetchResponse {
        status,
        headers,
        body: Box::pin(response.body.map(|chunk| {
            chunk
                .map(|bytes| bytes.to_vec())
                .map_err(|_| PluginError::transport("The remote gateway response stopped."))
        })),
    })
}
#[cfg(test)]
#[path = "../test/transport.test.rs"]
mod tests;
