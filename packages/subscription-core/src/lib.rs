use async_trait::async_trait;
use base64::Engine;
use futures::StreamExt;
use nora_ai::{HttpClient, HttpMethod, HttpRequest, HttpStreamingResponse, ReqwestHttpClient};
use nora_provider_plugin_sdk::{
    FetchRequest, FetchResponse, InitializeParams, NetworkProxy, PluginError, PluginResult,
    API_VERSION,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::sync::Mutex;

pub const MAX_JSON: usize = 16 * 1024 * 1024;
pub const MAX_MODELS: usize = 4096;
#[async_trait]
pub trait CredentialStore: Send + Sync {
    async fn save(&self, credentials: Value) -> PluginResult<()>;
}
struct HostStore;
#[async_trait]
impl CredentialStore for HostStore {
    async fn save(&self, credentials: Value) -> PluginResult<()> {
        nora_provider_plugin_sdk::save_credentials(credentials).await
    }
}
#[derive(Clone)]
pub struct Config {
    pub manifest: Value,
    pub proxy: Option<NetworkProxy>,
}
pub struct Core {
    pub id: &'static str,
    config: RwLock<Option<Config>>,
    client: Option<Arc<dyn HttpClient>>,
    pub pending: Mutex<BTreeMap<String, Value>>,
    pub refresh_lock: Mutex<()>,
    rotated: Mutex<BTreeMap<String, (u64, Value, bool)>>,
    store: Arc<dyn CredentialStore>,
}
impl Core {
    pub fn new(id: &'static str) -> Self {
        Self {
            id,
            config: RwLock::new(None),
            client: None,
            pending: Mutex::new(BTreeMap::new()),
            refresh_lock: Mutex::new(()),
            rotated: Mutex::new(BTreeMap::new()),
            store: Arc::new(HostStore),
        }
    }
    pub fn with_dependencies(
        id: &'static str,
        client: Arc<dyn HttpClient>,
        store: Arc<dyn CredentialStore>,
    ) -> Self {
        Self {
            client: Some(client),
            store,
            ..Self::new(id)
        }
    }
    pub fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        if params.api_version != API_VERSION
            || params.plugin_id != self.id
            || params.manifest["id"] != self.id
        {
            return Err(PluginError::invalid(
                "This executable requires its own provider manifest.",
            ));
        }
        let proxy: Option<NetworkProxy> = serde_json::from_value(
            params
                .manifest
                .pointer("/config/networkProxy")
                .cloned()
                .unwrap_or(Value::Null),
        )
        .map_err(|_| PluginError::invalid("The provider network settings are invalid."))?;
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        *self.config.write().map_err(|_| unavailable())? = Some(Config {
            manifest: params.manifest,
            proxy,
        });
        Ok(
            json!({"apiVersion":API_VERSION,"capabilities":{"models":true,"usage":true,"fetch":true,"persistent":true}}),
        )
    }
    pub fn config(&self) -> PluginResult<Config> {
        self.config
            .read()
            .map_err(|_| unavailable())?
            .clone()
            .ok_or_else(|| PluginError::invalid("Initialize the provider first."))
    }
    pub fn client(&self, proxy: Option<&NetworkProxy>) -> PluginResult<Arc<dyn HttpClient>> {
        let config = self.config()?;
        let proxy = proxy.or(config.proxy.as_ref());
        if let Some(proxy) = proxy {
            proxy.validate()?;
        }
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        Ok(Arc::new(
            ReqwestHttpClient::with_client(nora_provider_network::http_client(proxy)?)
                .with_observer_fn(|| async {
                    nora_provider_plugin_sdk::before_http_request()
                        .await
                        .map_err(|error| nora_ai::HttpClientError::Transport(error.to_string()))
                }),
        ))
    }
    pub fn params_client(&self, params: &Value) -> PluginResult<Arc<dyn HttpClient>> {
        let proxy: Option<NetworkProxy> =
            serde_json::from_value(params.get("networkProxy").cloned().unwrap_or(Value::Null))
                .map_err(|_| PluginError::invalid("The account network settings are invalid."))?;
        self.client(proxy.as_ref())
    }
    pub async fn begin(&self, mut pending: Value, until: u64) -> PluginResult<String> {
        let key = uuid::Uuid::new_v4().to_string();
        pending["until"] = json!(until.min(now().saturating_add(15 * 60_000)));
        let mut sessions = self.pending.lock().await;
        sessions.retain(|_, item| item["until"].as_u64().unwrap_or(0) > now());
        if sessions.len() >= 16 {
            return Err(PluginError::invalid(
                "Too many pending sign-ins. Finish or cancel an earlier sign-in.",
            ));
        }
        sessions.insert(key.clone(), pending);
        Ok(key)
    }
    pub async fn take_pending(&self, params: &Value) -> PluginResult<Value> {
        let key = required(params, "session")?;
        self.pending
            .lock()
            .await
            .remove(key)
            .filter(|value| value["until"].as_u64().unwrap_or(0) > now())
            .ok_or_else(|| PluginError::authentication("The sign-in expired. Start again."))
    }
    pub async fn latest(&self, old: &Value) -> PluginResult<Value> {
        let mut current = old.clone();
        let mut rotations = self.rotated.lock().await;
        rotations.retain(|_, (at, _, _)| now().saturating_sub(*at) < 24 * 3600_000);
        for _ in 0..32 {
            let key = fingerprint(&current);
            let Some((at, next, acknowledged)) = rotations.get(&key).cloned() else {
                return Ok(current);
            };
            if !acknowledged {
                self.store.save(next.clone()).await?;
                rotations.insert(key, (at, next.clone(), true));
            }
            current = next;
        }
        Err(PluginError::authentication(
            "The saved sign-in changed too many times. Reconnect the account.",
        ))
    }
    pub async fn persist_rotation(&self, old: &Value, next: Value) -> PluginResult<Value> {
        let key = fingerprint(old);
        let mut rotations = self.rotated.lock().await;
        if rotations.len() >= 64 {
            rotations.retain(|_, (at, _, _)| now().saturating_sub(*at) < 60_000);
        }
        if rotations.len() >= 64 {
            return Err(PluginError::transport("Too many unsaved token renewals."));
        }
        rotations.insert(key.clone(), (now(), next.clone(), false));
        self.store.save(next.clone()).await?;
        rotations.insert(key, (now(), next.clone(), true));
        Ok(next)
    }
}
fn unavailable() -> PluginError {
    PluginError::invalid("The provider configuration is unavailable.")
}
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn text(value: &Value) -> Option<&str> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}
pub fn required<'a>(value: &'a Value, name: &str) -> PluginResult<&'a str> {
    text(&value[name])
        .filter(|s| s.len() <= 64 * 1024 && !s.contains(['\r', '\n', '\0']))
        .ok_or_else(|| PluginError::invalid(format!("The {name} field is missing or invalid.")))
}
pub fn inputs(params: &Value) -> &Value {
    params
        .get("inputs")
        .or_else(|| params.get("fields"))
        .unwrap_or(params)
}
pub fn bearer(credentials: &Value) -> PluginResult<&str> {
    text(&credentials["access"])
        .or_else(|| text(&credentials["key"]))
        .or_else(|| text(&credentials["apiKey"]))
        .filter(|s| s.len() <= 64 * 1024 && !s.contains(['\r', '\n', '\0']))
        .ok_or_else(|| PluginError::authentication("Connect this account before using it."))
}
pub fn fingerprint(value: &Value) -> String {
    hex::encode(Sha256::digest(
        serde_json::to_vec(value).unwrap_or_default(),
    ))
}
pub fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}
pub fn timestamp(value: &Value) -> Option<u64> {
    if let Some(n) = value.as_u64() {
        return Some(if n < 100_000_000_000 {
            n.saturating_mul(1000)
        } else {
            n
        });
    }
    text(value)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .and_then(|d| u64::try_from(d.timestamp_millis()).ok())
}
pub fn iso(ms: u64) -> Value {
    chrono::DateTime::from_timestamp_millis(ms.min(i64::MAX as u64) as i64)
        .map(|d| json!(d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)))
        .unwrap_or(Value::Null)
}
pub fn json_request(
    method: HttpMethod,
    url: impl Into<String>,
    body: Option<&Value>,
    key: Option<&str>,
) -> PluginResult<HttpRequest> {
    let mut request = HttpRequest::new(method, url).with_header("accept", "application/json");
    if let Some(key) = key {
        request
            .headers
            .insert("authorization".into(), format!("Bearer {key}"));
    }
    if let Some(body) = body {
        request = request
            .with_json_body(body)
            .map_err(|_| PluginError::invalid("The request body cannot be encoded."))?;
    }
    Ok(request)
}
pub fn form_request(url: impl Into<String>, fields: &[(&str, &str)]) -> HttpRequest {
    HttpRequest::new(HttpMethod::Post, url)
        .with_header("accept", "application/json")
        .with_header("content-type", "application/x-www-form-urlencoded")
        .with_body(
            url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(fields.iter().copied())
                .finish()
                .into_bytes(),
        )
}
pub async fn exchange(client: &dyn HttpClient, request: HttpRequest) -> PluginResult<(u16, Value)> {
    let response = tokio::time::timeout(Duration::from_secs(30), client.stream(request))
        .await
        .map_err(|_| PluginError::transport("The provider did not answer in time."))?
        .map_err(|_| PluginError::transport("The provider could not be reached."))?;
    let status = response.status;
    let bytes = collect(response, MAX_JSON).await?;
    let value = serde_json::from_slice(&bytes).map_err(|_| {
        PluginError::transport(format!(
            "The provider returned invalid JSON (HTTP {status})."
        ))
    })?;
    Ok((status, value))
}
pub async fn checked(client: &dyn HttpClient, request: HttpRequest) -> PluginResult<Value> {
    let (status, value) = exchange(client, request).await?;
    if status == 401 || status == 403 {
        return Err(PluginError::authentication(format!(
            "The provider refused this account (HTTP {status})."
        )));
    }
    if !(200..300).contains(&status) {
        return Err(PluginError::transport(format!(
            "The provider returned HTTP {status}."
        )));
    }
    Ok(value)
}
pub async fn collect(mut response: HttpStreamingResponse, max: usize) -> PluginResult<Vec<u8>> {
    let mut output = Vec::new();
    while let Some(chunk) = tokio::time::timeout(Duration::from_secs(180), response.body.next())
        .await
        .map_err(|_| PluginError::transport("The provider response stopped."))?
    {
        let chunk = chunk
            .map_err(|_| PluginError::transport("The provider response ended unexpectedly."))?;
        if chunk.len() > max.saturating_sub(output.len()) {
            return Err(PluginError::transport(
                "The provider response exceeds its size limit.",
            ));
        }
        output.extend_from_slice(&chunk);
    }
    Ok(output)
}
pub async fn relay(client: &dyn HttpClient, request: HttpRequest) -> PluginResult<FetchResponse> {
    let response = tokio::time::timeout(Duration::from_secs(60), client.stream(request))
        .await
        .map_err(|_| PluginError::transport("The provider did not answer in time."))?
        .map_err(|_| PluginError::transport("The provider could not be reached."))?;
    let mut headers = response.headers;
    headers.retain(|name, _| {
        ![
            "content-length",
            "content-encoding",
            "transfer-encoding",
            "connection",
            "set-cookie",
        ]
        .contains(&name.to_ascii_lowercase().as_str())
    });
    let body = futures::stream::unfold(response.body, |mut stream| async move {
        match tokio::time::timeout(Duration::from_secs(180), stream.next()).await {
            Ok(Some(Ok(chunk))) => Some((Ok(chunk.to_vec()), stream)),
            Ok(Some(Err(_))) => Some((
                Err(PluginError::transport("The provider stream failed.")),
                Box::pin(futures::stream::empty()) as nora_ai::HttpByteStream,
            )),
            Ok(None) => None,
            Err(_) => Some((
                Err(PluginError::transport("The provider stream stopped.")),
                Box::pin(futures::stream::empty()) as nora_ai::HttpByteStream,
            )),
        }
    });
    Ok(FetchResponse {
        status: response.status,
        headers,
        body: Box::pin(body),
    })
}
pub fn request_body(request: &FetchRequest) -> PluginResult<Value> {
    let bytes = request
        .decode_body()?
        .ok_or_else(|| PluginError::invalid("The request body is missing."))?;
    if bytes.len() > MAX_JSON {
        return Err(PluginError::invalid(
            "This provider accepts JSON requests up to 16 MiB.",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| PluginError::invalid("The request body is invalid JSON."))
}
pub fn validate_fetch(
    request: &FetchRequest,
    base: &str,
    paths: &[&str],
) -> PluginResult<url::Url> {
    let url = url::Url::parse(&request.url)
        .map_err(|_| PluginError::invalid("The request address is invalid."))?;
    let base = url::Url::parse(base).map_err(|_| unavailable())?;
    if request.method != "POST"
        || url.origin() != base.origin()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !url.query_pairs().all(|(k, v)| k == "beta" && v == "true")
        || !paths.contains(&url.path())
    {
        return Err(PluginError::invalid(
            "This request is outside the provider's supported endpoints.",
        ));
    }
    Ok(url)
}
pub fn model(
    id: &str,
    name: &str,
    api: &str,
    provider: &str,
    base: &str,
    context: u64,
    output: u64,
    images: bool,
    reasoning: bool,
    efforts: Vec<String>,
    source: &str,
) -> Value {
    json!({"id":id,"name":name,"api":api,"provider":provider,"baseUrl":base,"contextWindow":context,"maxTokens":output,"input":if images {vec!["text","image"]}else{vec!["text"]},"reasoning":reasoning,"cost":{"input":0,"output":0},"info":{"chat":true,"toolCalling":true,"reasoningEfforts":efforts,"catalogSource":source}})
}
pub fn listing(models: Vec<Value>, api: &str, base: &str, source: &str) -> Value {
    json!({"models":models,"source":source,"allowUnknownModels":source=="preset","defaultAPI":api,"defaultBaseURL":base})
}
pub fn usage_item(
    label: &str,
    used: f64,
    limit: f64,
    resets: Option<u64>,
    span: Option<f64>,
    routing: bool,
) -> Value {
    let percent = if limit > 0.0 && used.is_finite() && limit.is_finite() {
        Some((used / limit * 100.0).max(0.0))
    } else {
        None
    };
    json!({"label":label,"used":used,"limit":limit,"usedPercent":percent,"resetsAt":resets.map(iso),"windowSeconds":span,"countsForRouting":routing})
}
pub fn saved_api(key: &str) -> Value {
    json!({"type":"api","authMethod":"apiKey","key":key,"expires":0})
}
pub fn decode_jwt(token: &str) -> Value {
    token
        .split('.')
        .nth(1)
        .and_then(|p| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(p)
                .ok()
        })
        .and_then(|p| serde_json::from_slice(&p).ok())
        .unwrap_or(Value::Null)
}

#[cfg(test)]
#[path = "../test/core.test.rs"]
mod tests;
