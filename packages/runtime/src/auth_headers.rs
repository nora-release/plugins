use crate::{auth, definition::Definition};
use futures::{stream, StreamExt};
use nora_ai::{HttpClient, HttpRequest, HttpStreamingResponse};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    future::Future,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

const ERROR_PREFIX: usize = 64 * 1024;
const CACHE_SIZE: usize = 512;
const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
type CacheKey = (String, String, String, [u8; 32]);
type Cache = BTreeMap<CacheKey, (&'static str, Instant)>;

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

pub(crate) fn relay(definition: &Definition, api: &str, target: &str) -> bool {
    if api != "anthropic-messages"
        || definition.config.endpoint_kind.as_deref() == Some("region")
        || definition.api == "azure-openai-responses"
    {
        return false;
    }
    let Ok(url) = url::Url::parse(target) else {
        return false;
    };
    let host = url.host_str().unwrap_or_default();
    !(host == "anthropic.com" || host.ends_with(".anthropic.com"))
}

fn explicit<'a>(definition: &'a Definition, credentials: &'a Value) -> Option<&'a str> {
    credentials
        .get("authHeader")
        .or_else(|| credentials.pointer("/extras/authHeader"))
        .and_then(Value::as_str)
        .or(Some(definition.config.auth_header.as_str()))
        .filter(|value| matches!(*value, "x-api-key" | "bearer"))
}

fn scope(definition: &Definition, credentials: &Value, target: &str) -> Option<CacheKey> {
    let account = definition.request_account.as_ref()?;
    let origin = url::Url::parse(target).ok()?.origin().ascii_serialization();
    let token = auth::decode(definition, Some(credentials))
        .ok()?
        .access_token;
    Some((
        definition.id.clone(),
        account.clone(),
        origin,
        Sha256::digest(token.as_bytes()).into(),
    ))
}

pub(crate) fn choice(
    definition: &Definition,
    credentials: &Value,
    target: &str,
) -> Option<&'static str> {
    if let Some(explicit) = explicit(definition, credentials) {
        return Some(if explicit == "bearer" {
            "bearer"
        } else {
            "x-api-key"
        });
    }
    let origin = url::Url::parse(target).ok()?.origin().ascii_serialization();
    let saved = credentials
        .pointer("/extras/noraLearnedAuthHeaders")
        .and_then(|value| value.get(&origin))
        .and_then(Value::as_str);
    if let Some(saved) = saved.filter(|value| matches!(*value, "bearer" | "x-api-key")) {
        return Some(if saved == "bearer" {
            "bearer"
        } else {
            "x-api-key"
        });
    }
    let key = scope(definition, credentials, target)?;
    let mut cache = cache().lock().ok()?;
    cache.retain(|_, (_, at)| at.elapsed() < CACHE_TTL);
    cache.get(&key).map(|(choice, _)| *choice)
}

pub(crate) fn apply(
    definition: &Definition,
    credentials: &Value,
    api: &str,
    target: &str,
    token: &str,
    headers: &mut BTreeMap<String, String>,
) {
    if token.is_empty() || !auth::is_api(credentials) || !relay(definition, api, target) {
        return;
    }
    headers.retain(|name, _| {
        !name.eq_ignore_ascii_case("authorization") && !name.eq_ignore_ascii_case("x-api-key")
    });
    let only = choice(definition, credentials, target);
    if only != Some("bearer") {
        headers.insert("x-api-key".into(), token.into());
    }
    if only != Some("x-api-key") {
        headers.insert("Authorization".into(), format!("Bearer {token}"));
    }
}

pub(crate) fn both_refused(status: u16, body: &[u8]) -> bool {
    if status != 401 {
        return false;
    }
    let body = String::from_utf8_lossy(body).to_ascii_lowercase();
    body.contains("x-api-key")
        && body.contains("authorization")
        && [
            "同时",
            "并存",
            "共存",
            "只能使用一个",
            "只能选择一个",
            "只能有一个",
            "cannot both",
            "can't both",
            "can not both",
            "cannot be both",
            "not both",
            "together",
            "at the same time",
            "simultaneous",
            "mutually exclusive",
            "only one of",
            "only one auth",
            "only one header",
            "both present",
            "both be present",
            "both provided",
            "both be provided",
        ]
        .iter()
        .any(|phrase| body.contains(phrase))
}

pub(crate) fn retryable(
    definition: &Definition,
    credentials: &Value,
    api: &str,
    request: &HttpRequest,
) -> bool {
    if !relay(definition, api, &request.url)
        || !auth::is_api(credentials)
        || explicit(definition, credentials).is_some()
        || definition.config.headers.keys().any(|name| {
            matches!(
                name.to_ascii_lowercase().as_str(),
                "authorization" | "x-api-key"
            )
        })
    {
        return false;
    }
    let Ok(decoded) = auth::decode(definition, Some(credentials)) else {
        return false;
    };
    let token = decoded.access_token;
    let header = |name: &str| {
        request
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };
    !token.is_empty()
        && header("x-api-key") == Some(token.as_str())
        && header("authorization") == Some(format!("Bearer {token}").as_str())
}

pub(crate) async fn remember(
    definition: &Definition,
    credentials: &Value,
    target: &str,
    only: &'static str,
) {
    let Some(key) = scope(definition, credentials, target) else {
        return;
    };
    if let Ok(mut cache) = cache().lock() {
        cache.retain(|_, (_, at)| at.elapsed() < CACHE_TTL);
        if cache.len() >= CACHE_SIZE {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(key, _)| key.clone())
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(key.clone(), (only, Instant::now()));
    }
    let mut updated = credentials.clone();
    if !updated.get("extras").is_some_and(Value::is_object) {
        updated["extras"] = json!({});
    }
    let mut choices = updated
        .pointer("/extras/noraLearnedAuthHeaders")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if choices.len() >= 16 && !choices.contains_key(&key.2) {
        choices.clear();
    }
    choices.insert(key.2, json!(only));
    updated["extras"]["noraLearnedAuthHeaders"] = Value::Object(choices);
    // Header selection is optional metadata; a host without credential ACKs
    // can still use the bounded in-process cache without changing credentials.
    let _ = nora_provider_plugin_sdk::save_credentials(updated).await;
}

async fn prefix(response: &mut HttpStreamingResponse) -> PluginResult<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut kept = Vec::new();
    while bytes.len() < ERROR_PREFIX {
        let Some(chunk) = response.body.next().await else {
            break;
        };
        match chunk {
            Ok(chunk) => {
                let length = chunk.len().min(ERROR_PREFIX - bytes.len());
                bytes.extend_from_slice(&chunk[..length]);
                kept.push(Ok(chunk));
            }
            Err(error) => {
                kept.push(Err(error));
                break;
            }
        }
    }
    let body = std::mem::replace(&mut response.body, Box::pin(stream::empty()));
    response.body = Box::pin(stream::iter(kept).chain(body));
    Ok(bytes)
}

pub(crate) async fn send(
    client: &dyn HttpClient,
    definition: &Definition,
    credentials: &Value,
    api: &str,
    request: HttpRequest,
) -> PluginResult<HttpStreamingResponse> {
    send_with(
        definition,
        credentials,
        api,
        request,
        |request| async move {
            client
                .stream(request)
                .await
                .map_err(|_| PluginError::transport("The provider endpoint could not be reached."))
        },
    )
    .await
}

pub(crate) async fn send_with<F, Fut>(
    definition: &Definition,
    credentials: &Value,
    api: &str,
    mut request: HttpRequest,
    mut send: F,
) -> PluginResult<HttpStreamingResponse>
where
    F: FnMut(HttpRequest) -> Fut,
    Fut: Future<Output = PluginResult<HttpStreamingResponse>>,
{
    if retryable(definition, credentials, api, &request) {
        if let Some(only) = choice(definition, credentials, &request.url) {
            one(&mut request.headers, only);
        }
    }
    let may_retry = retryable(definition, credentials, api, &request);
    if !may_retry {
        return send(request).await;
    }
    let mut response = send(request.clone()).await?;
    if response.status != 401 {
        return Ok(response);
    }
    let bytes = prefix(&mut response).await?;
    if !both_refused(response.status, &bytes) {
        return Ok(response);
    }
    drop(response);
    for only in ["x-api-key", "bearer"] {
        let mut next = request.clone();
        one(&mut next.headers, only);
        let response = send(next).await?;
        if !matches!(response.status, 401 | 403) {
            remember(definition, credentials, &request.url, only).await;
            return Ok(response);
        }
        if only == "bearer" {
            return Ok(response);
        }
    }
    unreachable!("The final single-header response is returned above")
}

fn one(headers: &mut BTreeMap<String, String>, only: &str) {
    headers.retain(|name, _| {
        if only == "bearer" {
            !name.eq_ignore_ascii_case("x-api-key")
        } else {
            !name.eq_ignore_ascii_case("authorization")
        }
    });
}

#[cfg(test)]
#[path = "../test/auth_headers.test.rs"]
mod tests;
