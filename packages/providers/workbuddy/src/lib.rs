//! WorkBuddy subscription protocol, adapted from magpie-community/plugins (MIT).
//! Browser accounts and explicit desktop sources use the supplier's own API.

mod auth;
mod checkin;
mod models;
mod request;
mod search;
mod stream;
mod tools;
mod usage;

use async_trait::async_trait;
use nora_ai::{HttpClient, HttpMethod, HttpRequest, HttpStreamingResponse, ReqwestHttpClient};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, PluginError,
    PluginResult, ProviderPlugin, API_VERSION,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Site {
    China,
    International,
}
impl Site {
    pub fn id(self) -> &'static str {
        match self {
            Self::China => "workbuddy",
            Self::International => "workbuddy-ai",
        }
    }
    fn endpoint(self) -> &'static str {
        match self {
            Self::China => "https://copilot.tencent.com",
            Self::International => "https://www.workbuddy.ai",
        }
    }
    fn base(self) -> String {
        format!("{}/v2", self.endpoint())
    }
    fn source_id(self) -> &'static str {
        match self {
            Self::China => "workbuddy-desktop",
            Self::International => "workbuddy-desktop-ai",
        }
    }
}

#[derive(Clone)]
struct Config {
    proxy: Option<NetworkProxy>,
    methods: Value,
}

pub struct WorkBuddy {
    site: Site,
    config: RwLock<Option<Config>>,
    pending: Mutex<BTreeMap<String, auth::Pending>>,
    source_tokens: Mutex<BTreeMap<String, Value>>,
    refreshed: Mutex<BTreeMap<String, (Value, u64)>>,
    learned: Mutex<request::Learned>,
    salt: String,
    client: Option<Arc<dyn HttpClient>>,
}

impl WorkBuddy {
    pub fn new(site: Site) -> Self {
        Self {
            site,
            config: RwLock::new(None),
            pending: Mutex::new(BTreeMap::new()),
            source_tokens: Mutex::new(BTreeMap::new()),
            refreshed: Mutex::new(BTreeMap::new()),
            learned: Mutex::new(request::Learned::default()),
            salt: random_id(),
            client: None,
        }
    }
    fn config(&self) -> PluginResult<Config> {
        self.config
            .read()
            .map_err(|_| invalid("Provider settings are unavailable."))?
            .clone()
            .ok_or_else(|| invalid("Initialize WorkBuddy before using it."))
    }
    fn client(&self, proxy: Option<&NetworkProxy>) -> PluginResult<Arc<dyn HttpClient>> {
        let config = self.config()?;
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        let client = nora_provider_plugin_sdk::http_client(proxy.or(config.proxy.as_ref()))?;
        Ok(Arc::new(
            ReqwestHttpClient::with_client(client).with_observer_fn(|| async {
                nora_provider_plugin_sdk::before_http_request()
                    .await
                    .map_err(|error| nora_ai::HttpClientError::Transport(error.to_string()))
            }),
        ))
    }
    fn params_client(&self, params: &Value) -> PluginResult<Arc<dyn HttpClient>> {
        let proxy: Option<NetworkProxy> =
            serde_json::from_value(params.get("networkProxy").cloned().unwrap_or(Value::Null))
                .map_err(|_| invalid("The account network settings are invalid."))?;
        self.client(proxy.as_ref())
    }
    async fn call(
        &self,
        client: &dyn HttpClient,
        method: HttpMethod,
        path: &str,
        headers: BTreeMap<String, String>,
        body: Option<Value>,
    ) -> PluginResult<Value> {
        let request = HttpRequest {
            method,
            url: format!("{}{path}", self.site.endpoint()),
            headers: headers
                .into_iter()
                .chain([
                    ("user-agent".into(), "WorkBuddy/5.5.6".into()),
                    ("content-type".into(), "application/json".into()),
                    ("accept".into(), "application/json".into()),
                ])
                .collect(),
            body: body.map(|body| body.to_string().into_bytes()),
        };
        envelope(send(client, request, Duration::from_secs(30)).await?).await
    }
}

#[async_trait]
impl ProviderPlugin for WorkBuddy {
    fn supports_web_search(&self) -> bool {
        true
    }
    async fn web_search(&self, context: CallContext, query: String) -> PluginResult<Value> {
        search::search(self, context, &query).await
    }
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        if params.api_version != API_VERSION
            || params.plugin_id != self.site.id()
            || params.manifest.get("id").and_then(Value::as_str) != Some(self.site.id())
        {
            return Err(invalid(
                "The WorkBuddy executable and provider manifest do not match.",
            ));
        }
        let proxy: Option<NetworkProxy> = serde_json::from_value(
            params
                .manifest
                .pointer("/contributes/provider/config/networkProxy")
                .cloned()
                .unwrap_or(Value::Null),
        )
        .map_err(|_| invalid("The network settings are invalid."))?;
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        *self
            .config
            .write()
            .map_err(|_| invalid("Provider settings are unavailable."))? = Some(Config {
            proxy,
            methods: params
                .manifest
                .pointer("/contributes/provider/authMethods")
                .cloned()
                .unwrap_or(json!([])),
        });
        let features = if self.site == Site::China {
            json!({"accountAutomation":1})
        } else {
            json!({})
        };
        Ok(
            json!({"apiVersion":API_VERSION,"capabilities":{"accountDiscovery":true,"sdkFeatures":features}}),
        )
    }
    async fn auth_methods(&self, _: CallContext) -> PluginResult<Value> {
        Ok(self.config()?.methods)
    }
    async fn discover_accounts(&self, _context: CallContext) -> PluginResult<Value> {
        auth::discover(self.site)
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        auth::authorize(self, params).await
    }
    async fn callback(&self, params: Value) -> PluginResult<Value> {
        auth::callback(self, params).await
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let credentials = params.get("credentials").ok_or_else(signed_out)?;
        if credentials.get("source").and_then(Value::as_str) == Some("desktop") {
            auth::read_source(self.site, credentials)?;
            return Ok(json!({"changed":false}));
        }
        let client = self.params_client(&params)?;
        let next = auth::renew(self, client.as_ref(), credentials).await?;
        // The host acknowledges the write before any caller can use the pair.
        nora_provider_plugin_sdk::save_credentials(next.clone()).await?;
        Ok(json!({"credentials":next}))
    }
    async fn accounts(&self, context: CallContext) -> PluginResult<Value> {
        let Some(credentials) = context.credentials else {
            return Ok(json!([]));
        };
        let mut saved = credentials.clone();
        if text(&saved, "source") == "desktop" {
            let current = auth::read_source(self.site, &saved)?;
            saved["enterpriseId"] = json!(text(&current, "enterpriseId"));
            saved["name"] = current["name"].clone();
        }
        auth::assign_identity(self.site, &mut saved)?;
        let mut result = json!({"accounts":[{"id":context.account,"name":saved.get("name"),"email":saved.get("email"),"configured":true}]});
        if saved != credentials {
            result["credentials"] = saved;
        }
        Ok(result)
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        let Some(credentials) = context.credentials else {
            return Ok(json!({"models":[],"source":"remote","allowUnknownModels":false}));
        };
        let client = self.client(context.network_proxy.as_ref())?;
        let credentials = auth::current(self, client.as_ref(), &credentials).await?;
        models::fetch(self, client.as_ref(), &credentials).await
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let credentials = context.credentials.as_ref().ok_or_else(signed_out)?;
        let client = self.client(context.network_proxy.as_ref())?;
        let credentials = auth::current(self, client.as_ref(), credentials).await?;
        usage::fetch(self, client.as_ref(), &credentials).await
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        request::fetch(self, request).await
    }
    async fn call(&self, method: &str, params: Value) -> PluginResult<Value> {
        if !matches!(method, "checkin" | "checkinStatus") {
            return Err(PluginError::unsupported(method));
        }
        let credentials = params.get("credentials").ok_or_else(signed_out)?;
        let client = self.params_client(&params)?;
        checkin::run(
            self,
            client.as_ref(),
            credentials,
            method == "checkinStatus",
        )
        .await
    }
}

fn invalid(message: &str) -> PluginError {
    PluginError::invalid(message)
}
fn signed_out() -> PluginError {
    PluginError::authentication("Sign in to this WorkBuddy account again.")
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
fn random_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).expect("The operating system random source is unavailable");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        .filter(|number| number.is_finite())
}
async fn send(
    client: &dyn HttpClient,
    request: HttpRequest,
    timeout: Duration,
) -> PluginResult<HttpStreamingResponse> {
    tokio::time::timeout(timeout, client.stream(request))
        .await
        .map_err(|_| PluginError::transport("The WorkBuddy request timed out."))?
        .map_err(|_| PluginError::transport("The WorkBuddy connection stopped."))
}
async fn bytes(
    response: HttpStreamingResponse,
    limit: usize,
) -> PluginResult<(u16, BTreeMap<String, String>, Vec<u8>)> {
    let status = response.status;
    let headers = response.headers;
    let body = tokio::time::timeout(
        Duration::from_secs(30),
        nora_ai::collect_http_body_text_limited(response.body, limit),
    )
    .await
    .map_err(|_| PluginError::transport("WorkBuddy did not finish its reply."))?
    .map_err(|_| PluginError::transport("WorkBuddy returned an incomplete or oversized reply."))?;
    Ok((status, headers, body.into_bytes()))
}
async fn envelope(response: HttpStreamingResponse) -> PluginResult<Value> {
    let (status, _, body) = bytes(response, 8 * 1024 * 1024).await?;
    if status == 401 {
        return Err(signed_out());
    }
    let value: Value = serde_json::from_slice(&body)
        .map_err(|_| PluginError::transport("WorkBuddy returned invalid JSON."))?;
    let code = number(value.get("code").unwrap_or(&Value::Null)).unwrap_or(0.0) as i64;
    if !(200..300).contains(&status) || code != 0 {
        return Err(PluginError::new(
            format!("workbuddy_{code}"),
            format!(
                "WorkBuddy returned HTTP {status}, code {code}: {}",
                text(&value, "msg")
            ),
        ));
    }
    value
        .get("data")
        .cloned()
        .ok_or_else(|| PluginError::transport("WorkBuddy returned no response data."))
}

#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
