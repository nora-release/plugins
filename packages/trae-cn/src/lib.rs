//! Native Trae provider protocol adapted from magpie-community/plugins (MIT).
//! The optional read-only desktop device codec derives from dsh-connect-trae (MIT).
mod auth;
mod desktop;
mod identity;
mod models;
mod request;
mod response;
mod site;
mod tools;
mod usage;
use async_trait::async_trait;
use nora_ai::{HttpClient, HttpMethod, HttpRequest, HttpStreamingResponse, ReqwestHttpClient};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, PluginError,
    PluginResult, ProviderPlugin, API_VERSION,
};
use serde_json::{json, Value};
pub use site::Site;
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::sync::Mutex;
#[derive(Clone)]
struct Config {
    proxy: Option<NetworkProxy>,
    methods: Value,
}
pub struct Trae {
    site: Site,
    config: RwLock<Option<Config>>,
    pending: Mutex<BTreeMap<String, auth::Pending>>,
    renewed: Mutex<BTreeMap<String, (Value, u64)>>,
    accounts: Mutex<BTreeMap<String, Arc<Mutex<models::State>>>>,
    client: Option<Arc<dyn HttpClient>>,
    salt: String,
}
impl Trae {
    pub fn new(site: Site) -> Self {
        Self {
            site,
            config: RwLock::new(None),
            pending: Mutex::new(BTreeMap::new()),
            renewed: Mutex::new(BTreeMap::new()),
            accounts: Mutex::new(BTreeMap::new()),
            client: None,
            salt: id(),
        }
    }
    fn config(&self) -> PluginResult<Config> {
        self.config
            .read()
            .map_err(|_| invalid("Trae settings are unavailable."))?
            .clone()
            .ok_or_else(|| invalid("Initialize the Trae provider first."))
    }
    fn client(&self, proxy: Option<&NetworkProxy>) -> PluginResult<Arc<dyn HttpClient>> {
        let config = self.config()?;
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        Ok(Arc::new(
            ReqwestHttpClient::with_client(nora_provider_network::http_client(
                proxy.or(config.proxy.as_ref()),
            )?)
            .with_observer_fn(|| async {
                nora_provider_plugin_sdk::before_http_request()
                    .await
                    .map_err(|error| nora_ai::HttpClientError::Transport(error.to_string()))
            }),
        ))
    }
    fn params_client(&self, params: &Value) -> PluginResult<Arc<dyn HttpClient>> {
        let proxy: Option<NetworkProxy> =
            serde_json::from_value(params.get("networkProxy").cloned().unwrap_or(Value::Null))
                .map_err(|_| invalid("The Trae account network settings are invalid."))?;
        self.client(proxy.as_ref())
    }
    async fn account(&self, credentials: &Value) -> PluginResult<Arc<Mutex<models::State>>> {
        let key = format!(
            "{}\0{}\0{}\0{}",
            self.site.id(),
            self.site.api(credentials),
            self.site.auth_host(credentials),
            text(credentials, "uid")
        );
        let mut accounts = self.accounts.lock().await;
        if let Some(account) = accounts.get(&key) {
            return Ok(account.clone());
        }
        if accounts.len() >= 128 {
            return Err(invalid("Too many Trae accounts are active."));
        }
        let state = Arc::new(Mutex::new(models::State::default()));
        accounts.insert(key, state.clone());
        Ok(state)
    }
}
#[async_trait]
impl ProviderPlugin for Trae {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        if params.api_version != API_VERSION
            || params.plugin_id != self.site.id()
            || params.manifest.get("id").and_then(Value::as_str) != Some(self.site.id())
        {
            return Err(invalid(
                "The Trae executable and provider manifest do not match.",
            ));
        }
        let proxy: Option<NetworkProxy> = serde_json::from_value(
            params
                .manifest
                .pointer("/config/networkProxy")
                .cloned()
                .unwrap_or(Value::Null),
        )
        .map_err(|_| invalid("The Trae network settings are invalid."))?;
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        *self
            .config
            .write()
            .map_err(|_| invalid("Trae settings are unavailable."))? = Some(Config {
            proxy,
            methods: params
                .manifest
                .get("authMethods")
                .cloned()
                .unwrap_or(json!([])),
        });
        let features = if self.site == Site::China {
            json!({"accountAutomation":1})
        } else {
            json!({})
        };
        Ok(json!({"apiVersion":API_VERSION,"capabilities":{"sdkFeatures":features}}))
    }
    async fn auth_methods(&self, _: CallContext) -> PluginResult<Value> {
        Ok(self.config()?.methods)
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        auth::authorize(self, params).await
    }
    async fn callback(&self, params: Value) -> PluginResult<Value> {
        auth::callback(self, params).await
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let credentials = credentials(params.get("credentials").ok_or_else(signed_out)?)?;
        let client = self.params_client(&params)?;
        let next = auth::renew(self, client.as_ref(), &credentials).await?;
        nora_provider_plugin_sdk::save_credentials(next.clone()).await?;
        Ok(json!({"credentials":next}))
    }
    async fn accounts(&self, context: CallContext) -> PluginResult<Value> {
        let Some(raw) = context.credentials else {
            return Ok(json!([]));
        };
        let mut current = credentials(&raw)?;
        identity::assign(self.site, &mut current)?;
        let mut result = json!({"accounts":[{"id":context.account,"accountId":current.get("accountId"),"email":current.get("email"),"name":current.get("accountId"),"configured":true}]});
        if current != raw {
            result["credentials"] = current;
        }
        Ok(result)
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        let Some(raw) = context.credentials else {
            return Ok(json!({"models":[],"source":"preset","allowUnknownModels":false}));
        };
        let credentials = credentials(&raw)?;
        let client = self.client(context.network_proxy.as_ref())?;
        let entries = models::fetch(self, client.as_ref(), &credentials, true).await?;
        let models = entries
            .iter()
            .map(|entry| entry.model(self.site, &credentials))
            .collect::<PluginResult<Vec<_>>>()?;
        Ok(
            json!({"models":models,"source":"remote","allowUnknownModels":false,"defaultAPI":"openai-completions","defaultBaseURL":format!("{}/v1",self.site.api(&credentials))}),
        )
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let credentials = credentials(context.credentials.as_ref().ok_or_else(signed_out)?)?;
        let client = self.client(context.network_proxy.as_ref())?;
        usage::fetch(self, client.as_ref(), &credentials).await
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        request::fetch(self, request).await
    }
    async fn call(&self, method: &str, params: Value) -> PluginResult<Value> {
        if !matches!(method, "checkin" | "checkinStatus") {
            return Err(PluginError::unsupported(method));
        }
        let credentials = credentials(params.get("credentials").ok_or_else(signed_out)?)?;
        let client = self.params_client(&params)?;
        if method == "checkinStatus" {
            usage::checkin_status(self, client.as_ref(), &credentials).await
        } else {
            usage::checkin(self, client.as_ref(), &credentials).await
        }
    }
}
fn invalid(message: impl Into<String>) -> PluginError {
    PluginError::invalid(message)
}
fn transport(message: impl Into<String>) -> PluginError {
    PluginError::transport(message)
}
fn signed_out() -> PluginError {
    PluginError::authentication("Sign in to Trae again.")
}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        .filter(|value| value.is_finite())
}
fn identifier(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.is_number().then(|| value.to_string()))
        .unwrap_or_default()
}
fn id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
fn credentials(value: &Value) -> PluginResult<Value> {
    if text(value, "access").is_empty()
        || text(value, "uid").is_empty()
        || text(value, "deviceId").is_empty()
        || text(value, "machineId").is_empty()
    {
        return Err(signed_out());
    }
    Ok(value.clone())
}
fn error_of(value: &Value) -> (String, String) {
    let error = value
        .pointer("/ResponseMetadata/Error")
        .filter(|error| error.get("Code").is_some() || error.get("Message").is_some())
        .or_else(|| value.get("error").filter(|error| error.is_object()))
        .unwrap_or(value);
    let code = error
        .get("code")
        .or_else(|| error.get("Code"))
        .or_else(|| value.get("code"))
        .map(identifier)
        .unwrap_or_default();
    let message = error
        .get("message")
        .or_else(|| error.get("Message"))
        .or_else(|| error.get("msg"))
        .or_else(|| value.get("error").filter(|error| error.is_string()))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    (code, message)
}
fn check(site: Site, status: u16, value: &Value) -> PluginResult<()> {
    let (code, message) = error_of(value);
    if status == 401 || site.lapsed(&code) {
        return Err(signed_out());
    }
    if status != 200 || !code.is_empty() && code != "0" {
        return Err(transport(format!(
            "{} returned HTTP {status}{}",
            site.name(),
            if message.is_empty() {
                String::new()
            } else {
                format!(": {message}")
            }
        )));
    }
    Ok(())
}
async fn json_request(client: &dyn HttpClient, request: HttpRequest) -> PluginResult<(u16, Value)> {
    tokio::time::timeout(Duration::from_secs(20), async {
        let response = client
            .stream(request)
            .await
            .map_err(|_| transport("The Trae service could not be reached."))?;
        let status = response.status;
        let body = nora_ai::collect_http_body_text_limited(response.body, 8 * 1024 * 1024)
            .await
            .map_err(|_| transport("Trae returned an incomplete or oversized response."))?;
        let value = match serde_json::from_str(&body) {
            Ok(value) => value,
            Err(_) if status >= 400 => json!({"message":format!("Trae returned HTTP {status}." )}),
            Err(_) => return Err(transport("Trae returned invalid JSON.")),
        };
        Ok((status, value))
    })
    .await
    .map_err(|_| transport("The Trae request timed out."))?
}
async fn post(
    client: &dyn HttpClient,
    url: String,
    credentials: &Value,
    body: Value,
) -> PluginResult<(u16, Value)> {
    json_request(
        client,
        HttpRequest {
            method: HttpMethod::Post,
            url,
            headers: site::ide_headers(credentials),
            body: Some(body.to_string().into_bytes()),
        },
    )
    .await
}
#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
