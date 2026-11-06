use nora_ai::{HttpClient, HttpMethod, HttpRequest, ResolvedProviderAuth};
use nora_provider::usage::parsing::{number, quantities, result};
use serde_json::{json, Value};
use std::fmt;
use std::time::Duration;

#[derive(Debug, PartialEq)]
pub enum QuotaError {
    Credentials,
    Transport,
    Status(u16),
    InvalidResponse,
}
impl fmt::Display for QuotaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Credentials => formatter.write_str("Connect an API key to read its allowance."),
            Self::Transport => formatter.write_str("The allowance endpoint could not be reached."),
            Self::Status(status) => {
                write!(formatter, "The allowance endpoint returned HTTP {status}.")
            }
            Self::InvalidResponse => {
                formatter.write_str("The provider returned an invalid allowance response.")
            }
        }
    }
}
impl std::error::Error for QuotaError {}

pub async fn fetch(
    auth: &ResolvedProviderAuth,
    host: &str,
    endpoint: &str,
    bearer: bool,
    client: &dyn HttpClient,
) -> Result<Option<Value>, QuotaError> {
    fetch_with_headers(auth, host, endpoint, bearer, &[], client).await
}

pub async fn fetch_with_headers(
    auth: &ResolvedProviderAuth,
    host: &str,
    endpoint: &str,
    bearer: bool,
    headers: &[(String, String)],
    client: &dyn HttpClient,
) -> Result<Option<Value>, QuotaError> {
    fetch_between(auth, host, host, endpoint, bearer, headers, client).await
}

pub async fn fetch_between(
    auth: &ResolvedProviderAuth,
    origin_host: &str,
    target_host: &str,
    endpoint: &str,
    bearer: bool,
    headers: &[(String, String)],
    client: &dyn HttpClient,
) -> Result<Option<Value>, QuotaError> {
    let Some(base) = auth
        .base_url
        .as_deref()
        .and_then(|url| url::Url::parse(url).ok())
    else {
        return Ok(None);
    };
    if base.scheme() != "https"
        || base.host_str() != Some(origin_host)
        || base.port_or_known_default() != Some(443)
        || !base.username().is_empty()
        || base.password().is_some()
    {
        return Ok(None);
    }
    let target = url::Url::parse(endpoint).map_err(|_| QuotaError::InvalidResponse)?;
    if target.scheme() != "https"
        || target.host_str() != Some(target_host)
        || target.port_or_known_default() != Some(443)
        || !target.username().is_empty()
        || target.password().is_some()
    {
        return Err(QuotaError::InvalidResponse);
    }
    let token = auth
        .token
        .as_deref()
        .filter(|token| !token.trim().is_empty() && !token.contains(['\r', '\n']))
        .ok_or(QuotaError::Credentials)?;
    let mut request = HttpRequest::new(HttpMethod::Get, endpoint)
        .with_header(
            "authorization",
            if bearer {
                format!("Bearer {token}")
            } else {
                token.into()
            },
        )
        .with_header("accept", "application/json")
        .with_header("accept-language", "en-US,en");
    for (name, value) in headers {
        request = request.with_header(name, value);
    }
    tokio::time::timeout(Duration::from_secs(20), async {
        let response = client
            .stream(request)
            .await
            .map_err(|_| QuotaError::Transport)?;
        if !(200..300).contains(&response.status) {
            return Err(QuotaError::Status(response.status));
        }
        let text = nora_ai::collect_http_body_text_limited(response.body, 1024 * 1024 + 1)
            .await
            .map_err(|_| QuotaError::InvalidResponse)?;
        if text.len() > 1024 * 1024 {
            return Err(QuotaError::InvalidResponse);
        }
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|_| QuotaError::InvalidResponse)
    })
    .await
    .map_err(|_| QuotaError::Transport)?
}

pub fn unsupported_origin() -> Value {
    nora_provider::usage::unavailable("Allowance queries require this provider's official endpoint. No API key was sent to another service.")
}

pub fn balance(id: &str, label: &str, amount: f64, currency: &str) -> Value {
    json!({"id":id,"label":label,"amount":amount,"unit":currency})
}

pub fn finish(items: Vec<Value>, balances: Vec<Value>) -> Result<Value, String> {
    if items.is_empty() && balances.is_empty() {
        return Err("The provider did not return allowance or balance totals.".into());
    }
    Ok(result(&json!({}), items, balances))
}

pub fn balance_at(payload: &Value, path: &str, currency: &str) -> Result<Value, String> {
    let amount = payload
        .pointer(path)
        .and_then(number)
        .ok_or("The provider did not return a readable balance.")?;
    finish(
        Vec::new(),
        vec![balance("balance", "Balance", amount, currency)],
    )
}

pub fn window(
    id: &str,
    label: &str,
    values: &Value,
    reset: Option<&Value>,
    span: Option<f64>,
    routing: bool,
    unit: Option<&str>,
) -> Option<Value> {
    let mut item = quantities(id, label, values, unit, reset)?;
    item["countsForRouting"] = json!(routing);
    if let Some(span) = span.filter(|span| span.is_finite() && *span > 0.0) {
        item["windowSeconds"] = json!(span);
    }
    Some(item)
}

#[cfg(test)]
#[path = "../test/quota.test.rs"]
mod tests;
