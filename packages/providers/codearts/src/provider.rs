use async_trait::async_trait;
use nora_ai::{HttpClient, HttpMethod, HttpRequest, HttpStreamingResponse, ReqwestHttpClient};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, ProviderPlugin,
    API_VERSION,
};
pub(super) use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::{Mutex, Semaphore};

pub(super) const SNAP: &str = "https://snap-access.cn-north-4.myhuaweicloud.com";
pub(super) const OPEN: &str = "https://opengw.developer.huaweicloud.com";
pub(super) const BASE: &str = "https://snap-access.cn-north-4.myhuaweicloud.com/api/v2";
pub(super) const STS: &str = "https://sts.cn-north-4.myhuaweicloud.com/v1/oauth2/tokens";

#[derive(Clone)]
struct Config {
    proxy: Option<NetworkProxy>,
    methods: Value,
    data: PathBuf,
    max_inflight: usize,
}

pub(super) struct Account {
    pub gate: Arc<Semaphore>,
    pub models: Mutex<crate::models::State>,
}

#[derive(Default)]
pub(super) struct CodeArts {
    config: RwLock<Option<Config>>,
    pub pending: Mutex<BTreeMap<String, crate::auth::Pending>>,
    accounts: Mutex<BTreeMap<String, Arc<Account>>>,
    pub refreshing: Mutex<BTreeMap<String, (Value, u64)>>,
    pub client: Option<Arc<dyn HttpClient>>,
}

impl CodeArts {
    fn config(&self) -> PluginResult<Config> {
        self.config
            .read()
            .map_err(|_| invalid("CodeArts settings are unavailable."))?
            .clone()
            .ok_or_else(|| invalid("Initialize CodeArts before using it."))
    }
    pub fn client(&self, proxy: Option<&NetworkProxy>) -> PluginResult<Arc<dyn HttpClient>> {
        let config = self.config()?;
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        Ok(Arc::new(
            ReqwestHttpClient::with_client(nora_provider_plugin_sdk::http_client(
                proxy.or(config.proxy.as_ref()),
            )?)
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
                .map_err(|_| invalid("The CodeArts account network settings are invalid."))?;
        self.client(proxy.as_ref())
    }
    pub fn data(&self) -> PluginResult<PathBuf> {
        Ok(self.config()?.data)
    }
    pub async fn account(&self, credentials: &Value) -> PluginResult<Arc<Account>> {
        let key = session(credentials).0;
        let mut accounts = self.accounts.lock().await;
        if let Some(account) = accounts.get(&key) {
            return Ok(account.clone());
        }
        if accounts.len() >= 128 {
            return Err(invalid("Too many CodeArts accounts are active."));
        }
        let account = Arc::new(Account {
            gate: Arc::new(Semaphore::new(self.config()?.max_inflight)),
            models: Mutex::new(crate::models::State::default()),
        });
        accounts.insert(key, account.clone());
        Ok(account)
    }
    pub async fn signed(
        &self,
        client: &dyn HttpClient,
        credentials: &Value,
        method: HttpMethod,
        url: &str,
        body: Option<Value>,
        extra: BTreeMap<String, String>,
        unsigned: BTreeMap<String, String>,
    ) -> PluginResult<HttpStreamingResponse> {
        let bytes = body
            .map(|body| body.to_string().into_bytes())
            .unwrap_or_default();
        let headers = crate::sign::huawei(
            credentials,
            method.as_str(),
            url,
            &bytes,
            extra,
            &chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string(),
        )?;
        send(
            client,
            HttpRequest {
                method,
                url: url.into(),
                headers: headers.into_iter().chain(unsigned).collect(),
                body: (!bytes.is_empty()).then_some(bytes),
            },
            Duration::from_secs(30),
        )
        .await
    }
    pub async fn get(
        &self,
        client: &dyn HttpClient,
        credentials: &Value,
        url: &str,
        headers: BTreeMap<String, String>,
    ) -> PluginResult<Value> {
        json_body(
            self.signed(
                client,
                credentials,
                HttpMethod::Get,
                url,
                None,
                BTreeMap::new(),
                headers,
            )
            .await?,
        )
        .await
    }
    pub async fn snap(
        &self,
        client: &dyn HttpClient,
        credentials: &Value,
        method: HttpMethod,
        url: &str,
        body: Option<Value>,
    ) -> PluginResult<Value> {
        let value = json_body(
            self.signed(
                client,
                credentials,
                method,
                url,
                body,
                BTreeMap::new(),
                snap_headers(),
            )
            .await?,
        )
        .await?;
        if let Some(code) = value.get("code").filter(|code| !code.is_null()) {
            if number(code).unwrap_or(-1.0) != 0.0 {
                return Err(PluginError::transport(format!(
                    "CodeArts declined this request (code {}).",
                    code
                )));
            }
        }
        Ok(value.get("data").cloned().unwrap_or(value))
    }
}

#[async_trait]
impl ProviderPlugin for CodeArts {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        if params.api_version != API_VERSION
            || params.plugin_id != "codearts"
            || params.manifest.get("id").and_then(Value::as_str) != Some("codearts")
            || !params.data_directory.is_absolute()
        {
            return Err(invalid(
                "The CodeArts executable and manifest do not match.",
            ));
        }
        let proxy: Option<NetworkProxy> = serde_json::from_value(
            params
                .manifest
                .pointer("/contributes/provider/config/networkProxy")
                .cloned()
                .unwrap_or(Value::Null),
        )
        .map_err(|_| invalid("The CodeArts network settings are invalid."))?;
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        *self
            .config
            .write()
            .map_err(|_| invalid("CodeArts settings are unavailable."))? = Some(Config {
            proxy,
            methods: params
                .manifest
                .pointer("/contributes/provider/authMethods")
                .cloned()
                .unwrap_or(json!([])),
            data: params.data_directory,
            max_inflight: params
                .manifest
                .pointer("/contributes/provider/config/maxInflight")
                .and_then(Value::as_u64)
                .filter(|limit| *limit > 0)
                .map(|limit| limit.min(3) as usize)
                .unwrap_or(3),
        });
        Ok(json!({"apiVersion":API_VERSION}))
    }
    async fn auth_methods(&self, _: CallContext) -> PluginResult<Value> {
        Ok(self.config()?.methods)
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        crate::auth::authorize(self, params).await
    }
    async fn callback(&self, params: Value) -> PluginResult<Value> {
        crate::auth::callback(self, params).await
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let previous = decode(params.get("credentials").ok_or_else(signed_out)?)?;
        let client = self.params_client(&params)?;
        let next = crate::auth::renew(self, client.as_ref(), &previous).await?;
        let credentials = encode(next)?;
        nora_provider_plugin_sdk::save_credentials(credentials.clone()).await?;
        Ok(json!({"credentials":credentials}))
    }
    async fn accounts(&self, context: CallContext) -> PluginResult<Value> {
        let Some(raw) = context.credentials else {
            return Ok(json!([]));
        };
        let credentials = decode(&raw)?;
        let (id, name) = identity(&credentials);
        Ok(json!([{"id":context.account,"accountId":id,"name":name,"configured":true}]))
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        let Some(raw) = context.credentials else {
            return Ok(json!({"models":[],"source":"remote","allowUnknownModels":false}));
        };
        let credentials = decode(&raw)?;
        let client = self.client(context.network_proxy.as_ref())?;
        crate::models::fetch(self, client.as_ref(), &credentials).await
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let credentials = decode(context.credentials.as_ref().ok_or_else(signed_out)?)?;
        let client = self.client(context.network_proxy.as_ref())?;
        crate::usage::fetch(self, client.as_ref(), &credentials).await
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        crate::gateway::fetch(self, request).await
    }
    async fn call(&self, method: &str, params: Value) -> PluginResult<Value> {
        if method != "checkin" {
            return Err(PluginError::unsupported(method));
        }
        let credentials = decode(params.get("credentials").ok_or_else(signed_out)?)?;
        let client = self.params_client(&params)?;
        crate::usage::checkin(self, client.as_ref(), &credentials).await
    }
}

pub(super) fn invalid(message: &str) -> PluginError {
    PluginError::invalid(message)
}
pub(super) fn signed_out() -> PluginError {
    PluginError::authentication("Sign in to CodeArts with Huawei Cloud again.")
}
pub(super) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
pub(super) fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
pub(super) fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        .filter(|value| value.is_finite())
}
pub(super) fn decode(value: &Value) -> PluginResult<Value> {
    let value: Value = serde_json::from_str(text(value, "access")).map_err(|_| signed_out())?;
    for key in ["access_key_id", "secret_access_key", "security_token"] {
        if text(&value, key).is_empty() {
            return Err(signed_out());
        }
    }
    Ok(value)
}
pub(super) fn encode(value: Value) -> PluginResult<Value> {
    let expires = crate::usage::date(value.get("expires_at").unwrap_or(&Value::Null))
        .ok_or_else(|| invalid("CodeArts did not return a valid credential expiration."))?;
    let (id, name) = identity(&value);
    Ok(
        json!({"type":"oauth","access":value.to_string(),"refresh":value.get("refresh_token"),"expires":expires,"accountId":id,"name":name}),
    )
}
pub(super) fn identity(credentials: &Value) -> (String, String) {
    let payload = text(credentials, "refresh_token")
        .split('.')
        .nth(1)
        .and_then(encoded_json);
    let raw = payload
        .as_ref()
        .and_then(|payload| payload.get("user_profile"));
    let profile = raw
        .and_then(Value::as_str)
        .and_then(|text| {
            serde_json::from_str(text)
                .ok()
                .or_else(|| encoded_json(text))
        })
        .or_else(|| raw.cloned())
        .unwrap_or(Value::Null);
    let id = ["principal_urn", "account_name", "account_id", "user_id"]
        .into_iter()
        .map(|key| text(&profile, key))
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| {
            if text(credentials, "user_name").is_empty() {
                text(credentials, "user_id")
            } else {
                text(credentials, "user_name")
            }
        });
    let name = ["user_name", "account_name", "domain_name"]
        .into_iter()
        .map(|key| text(&profile, key))
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| text(credentials, "user_name"));
    (id.into(), name.into())
}
fn encoded_json(value: &str) -> Option<Value> {
    use base64::{engine::general_purpose, Engine};
    [
        general_purpose::URL_SAFE_NO_PAD,
        general_purpose::URL_SAFE,
        general_purpose::STANDARD_NO_PAD,
        general_purpose::STANDARD,
    ]
    .iter()
    .find_map(|engine| {
        engine
            .decode(value)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    })
}
pub(super) fn session(credentials: &Value) -> (String, String) {
    let seed = if !text(credentials, "session_id").is_empty() {
        text(credentials, "session_id").to_owned()
    } else {
        let (id, _) = identity(credentials);
        if !id.is_empty() {
            id
        } else {
            text(credentials, "access_key_id").into()
        }
    };
    let digest = crate::sign::digest(format!("codearts-session:{seed}").as_bytes());
    (digest[..32].into(), digest[32..].into())
}
pub(super) fn snap_headers() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("agent-type".into(), "PromptCenter".into()),
        ("x-language".into(), "zh-cn".into()),
    ])
}
pub(super) async fn send(
    client: &dyn HttpClient,
    request: HttpRequest,
    timeout: Duration,
) -> PluginResult<HttpStreamingResponse> {
    tokio::time::timeout(timeout, client.stream(request))
        .await
        .map_err(|_| PluginError::transport("The CodeArts request timed out."))?
        .map_err(|_| PluginError::transport("The CodeArts service could not be reached."))
}
pub(super) async fn body(
    response: HttpStreamingResponse,
    limit: usize,
) -> PluginResult<(u16, BTreeMap<String, String>, Vec<u8>)> {
    let status = response.status;
    let headers = response.headers;
    let text = tokio::time::timeout(
        Duration::from_secs(60),
        nora_ai::collect_http_body_text_limited(response.body, limit),
    )
    .await
    .map_err(|_| PluginError::transport("The CodeArts response timed out."))?
    .map_err(|_| {
        PluginError::transport("CodeArts returned an incomplete or oversized response.")
    })?;
    Ok((status, headers, text.into_bytes()))
}
pub(super) async fn json_body(response: HttpStreamingResponse) -> PluginResult<Value> {
    let (status, _, body) = body(response, 8 * 1024 * 1024).await?;
    if status == 401 {
        return Err(signed_out());
    }
    if !(200..300).contains(&status) {
        return Err(PluginError::transport(format!(
            "CodeArts returned HTTP {status}."
        )));
    }
    serde_json::from_slice(&body)
        .map_err(|_| PluginError::transport("CodeArts returned invalid JSON."))
}

#[cfg(test)]
#[path = "../test/provider.test.rs"]
pub(crate) mod tests;
