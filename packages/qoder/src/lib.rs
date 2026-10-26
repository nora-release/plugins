//! Native Qoder protocol adapted from magpie-community/plugins (MIT), whose
//! COSY transport derives from ufec/CLIProxyAPI (MIT). See bundled LICENSES.
mod auth;
mod codec;
mod identity;
mod models;
mod queue;
mod request;
mod response;
mod schema;
mod site;
mod usage;
mod work_usage;
use async_trait::async_trait;
use nora_ai::{HttpClient, HttpMethod, HttpRequest, HttpStreamingResponse, ReqwestHttpClient};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, PluginError,
    PluginResult, ProviderPlugin, API_VERSION,
};
use serde_json::{json, Value};
use site::Deployment;
pub use site::Site;
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::sync::Mutex;

pub(crate) const VERSION: &str = "1.1.69";
pub(crate) const MODEL_PATH: &str = "/algo/api/v2/model/list?Encode=1";
pub(crate) const CHAT_PATH:&str="/algo/api/v2/service/pro/sse/agent_chat_generation?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1";
#[derive(Clone)]
struct Config {
    proxy: Option<NetworkProxy>,
    methods: Value,
}
pub struct Qoder {
    site: Site,
    config: RwLock<Option<Config>>,
    pending: Mutex<BTreeMap<String, auth::Flow>>,
    spent: Mutex<BTreeMap<String, (Value, u64)>>,
    listings: Mutex<BTreeMap<String, (Vec<models::Entry>, u64)>>,
    machines: Mutex<BTreeMap<String, (BTreeMap<String, String>, u64)>>,
    client: Option<Arc<dyn HttpClient>>,
}
impl Qoder {
    pub fn new(site: Site) -> Self {
        Self {
            site,
            config: RwLock::new(None),
            pending: Mutex::new(BTreeMap::new()),
            spent: Mutex::new(BTreeMap::new()),
            listings: Mutex::new(BTreeMap::new()),
            machines: Mutex::new(BTreeMap::new()),
            client: None,
        }
    }
    fn config(&self) -> PluginResult<Config> {
        self.config
            .read()
            .map_err(|_| invalid("Qoder settings are unavailable."))?
            .clone()
            .ok_or_else(|| invalid("Initialize the Qoder provider first."))
    }
    fn client(&self, proxy: Option<&NetworkProxy>) -> PluginResult<Arc<dyn HttpClient>> {
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
    fn params_client(&self, params: &Value) -> PluginResult<Arc<dyn HttpClient>> {
        let proxy: Option<NetworkProxy> =
            serde_json::from_value(params.get("networkProxy").cloned().unwrap_or(Value::Null))
                .map_err(|_| invalid("The Qoder account network settings are invalid."))?;
        self.client(proxy.as_ref())
    }
    fn deployment(&self, credentials: &Value) -> PluginResult<Deployment> {
        if credentials
            .get("vpc")
            .is_some_and(|value| !value.is_null() && !value.is_string())
        {
            return Err(authentication(
                "The Qoder enterprise account has an invalid instance. Sign in again.",
            ));
        }
        if self.site == Site::QwenWorkEnterprise {
            let host = credentials
                .get("enterpriseHost")
                .and_then(Value::as_str)
                .or_else(|| credentials.get("vpc").and_then(Value::as_str))
                .filter(|host| !host.is_empty())
                .ok_or_else(|| {
                    authentication(
                        "This QwenWork enterprise account has no original domain. Sign in again.",
                    )
                })?;
            if credentials
                .get("vpc")
                .and_then(Value::as_str)
                .is_some_and(|vpc| vpc != host)
            {
                return Err(authentication(
                    "The QwenWork enterprise account domain changed. Sign in again.",
                ));
            }
            return self.site.deployment(Some(host));
        }
        self.site
            .deployment(credentials.get("vpc").and_then(Value::as_str))
    }
    async fn listing(
        &self,
        client: &dyn HttpClient,
        credentials: &Value,
        again: bool,
    ) -> PluginResult<Vec<models::Entry>> {
        let deployment = self.deployment(credentials)?;
        let key = format!("{}\0{}", deployment.api, text(credentials, "uid"));
        let mut listings = self.listings.lock().await;
        if !again {
            if let Some((entries, at)) = listings.get(&key) {
                if now().saturating_sub(*at) < 300_000 {
                    return Ok(entries.clone());
                }
            }
        }
        let url = format!("{}{MODEL_PATH}", deployment.api);
        let headers = codec::headers_for(self.site, &url, credentials, "")?;
        let response = send(
            client,
            HttpRequest {
                method: HttpMethod::Get,
                url,
                headers,
                body: None,
            },
            15,
        )
        .await?;
        let (status, _, value) = read(response, 8 * 1024 * 1024).await?;
        if status == 401 || status == 403 {
            return Err(authentication(
                "Qoder refused the model list. Renew the account sign-in.",
            ));
        }
        let value: Value = if status == 503 && self.site == Site::QwenWorkEnterprise {
            work_usage::fallback_catalog()
        } else {
            if status != 200 {
                return Err(transport(format!(
                    "{} models returned HTTP {status}.",
                    self.site.id()
                )));
            }
            serde_json::from_slice(&value)
                .map_err(|_| transport("The provider returned invalid model JSON."))?
        };
        let entries = models::listing_for(&value, self.site)?;
        if listings.len() >= 128 && !listings.contains_key(&key) {
            if let Some(first) = listings
                .iter()
                .min_by_key(|(_, (_, at))| at)
                .map(|(key, _)| key.clone())
            {
                listings.remove(&first);
            }
        }
        listings.insert(key, (entries.clone(), now()));
        Ok(entries)
    }
}
#[async_trait]
impl ProviderPlugin for Qoder {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        if params.api_version != API_VERSION
            || params.plugin_id != self.site.id()
            || params.manifest.get("id").and_then(Value::as_str) != Some(self.site.id())
        {
            return Err(invalid(
                "The Qoder executable and provider manifest do not match.",
            ));
        }
        let proxy: Option<NetworkProxy> = serde_json::from_value(
            params
                .manifest
                .pointer("/config/networkProxy")
                .cloned()
                .unwrap_or(Value::Null),
        )
        .map_err(|_| invalid("The Qoder network settings are invalid."))?;
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        *self
            .config
            .write()
            .map_err(|_| invalid("Qoder settings are unavailable."))? = Some(Config {
            proxy,
            methods: params
                .manifest
                .get("authMethods")
                .cloned()
                .unwrap_or(json!([])),
        });
        Ok(json!({"apiVersion":API_VERSION,"capabilities":{"sdkFeatures":{"accountAutomation":1}}}))
    }
    async fn auth_methods(&self, _: CallContext) -> PluginResult<Value> {
        let mut methods = self.config()?.methods;
        if self.site != Site::QwenWorkEnterprise {
            return Ok(methods);
        }
        let Some(domain) = auth::enterprise_domain_preset() else {
            return Ok(methods);
        };
        for method in methods.as_array_mut().into_iter().flatten() {
            if method["id"] != "oauth" {
                continue;
            }
            method["prompts"] = json!([]);
            if let Some(label) = method["label"].as_str() {
                method["label"] = json!(format!("{label} ({domain})"));
            }
        }
        Ok(methods)
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        auth::authorize(self, params).await
    }
    async fn callback(&self, params: Value) -> PluginResult<Value> {
        auth::callback(self, params).await
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let credentials = checked(params.get("credentials").ok_or_else(signed_out)?)?;
        let client = self.params_client(&params)?;
        let next = auth::renew(self, client.as_ref(), &credentials, false).await?;
        nora_provider_plugin_sdk::save_credentials(next.clone()).await?;
        Ok(json!({"credentials":next}))
    }
    async fn accounts(&self, context: CallContext) -> PluginResult<Value> {
        let Some(credentials) = context.credentials else {
            return Ok(json!([]));
        };
        let mut current = checked(&credentials)?;
        identity::assign(&mut current, &self.deployment(&credentials)?)?;
        let mut result = json!({"accounts":[{"id":context.account,"accountId":current.get("accountId"),"email":current.get("email"),"name":current.get("name"),"configured":true}]});
        if current != credentials {
            result["credentials"] = current;
        }
        Ok(result)
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        let Some(credentials) = context.credentials else {
            return Ok(json!({"models":[],"source":"remote","allowUnknownModels":false}));
        };
        let credentials = checked(&credentials)?;
        let client = self.client(context.network_proxy.as_ref())?;
        let credentials = auth::fresh(self, client.as_ref(), &credentials).await?;
        let entries = self.listing(client.as_ref(), &credentials, true).await?;
        let deployment = self.deployment(&credentials)?;
        let models = entries
            .iter()
            .map(|entry| entry.model(self.site, &deployment))
            .collect::<PluginResult<Vec<_>>>()?;
        Ok(
            json!({"models":models,"source":"remote","allowUnknownModels":false,"defaultAPI":"openai-completions","defaultBaseURL":deployment.api}),
        )
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let credentials = checked(context.credentials.as_ref().ok_or_else(signed_out)?)?;
        let client = self.client(context.network_proxy.as_ref())?;
        if self.site.is_work() {
            return work_usage::fetch(self, client.as_ref(), credentials).await;
        }
        usage::fetch(self, client.as_ref(), credentials).await
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        request::fetch(self, request).await
    }
    async fn call(&self, method: &str, params: Value) -> PluginResult<Value> {
        if self.site.is_work() || !matches!(method, "checkin" | "checkinStatus") {
            return Err(PluginError::unsupported(method));
        }
        let credentials = checked(params.get("credentials").ok_or_else(signed_out)?)?;
        let client = self.params_client(&params)?;
        if method == "checkinStatus" {
            usage::checkin_status(self, client.as_ref(), credentials).await
        } else {
            usage::checkin(self, client.as_ref(), credentials).await
        }
    }
}
fn invalid(message: impl Into<String>) -> PluginError {
    PluginError::invalid(message)
}
fn transport(message: impl Into<String>) -> PluginError {
    PluginError::transport(message)
}
fn authentication(message: impl Into<String>) -> PluginError {
    PluginError::authentication(message)
}
fn signed_out() -> PluginError {
    authentication("Sign in to Qoder again.")
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
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
fn id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}
fn checked(value: &Value) -> PluginResult<Value> {
    if text(value, "access").is_empty()
        || text(value, "uid").is_empty()
        || text(value, "machineId").is_empty()
    {
        return Err(signed_out());
    }
    Ok(value.clone())
}
async fn send(
    client: &dyn HttpClient,
    request: HttpRequest,
    seconds: u64,
) -> PluginResult<HttpStreamingResponse> {
    tokio::time::timeout(Duration::from_secs(seconds), client.stream(request))
        .await
        .map_err(|_| transport("The Qoder request timed out."))?
        .map_err(|_| transport("The Qoder service could not be reached."))
}
async fn read(
    response: HttpStreamingResponse,
    limit: usize,
) -> PluginResult<(u16, BTreeMap<String, String>, Vec<u8>)> {
    let status = response.status;
    let headers = response.headers;
    let text = tokio::time::timeout(
        Duration::from_secs(30),
        nora_ai::collect_http_body_text_limited(response.body, limit),
    )
    .await
    .map_err(|_| transport("The Qoder response timed out."))?
    .map_err(|_| transport("Qoder returned an incomplete or oversized response."))?;
    Ok((status, headers, text.into_bytes()))
}
async fn openapi(
    client: &dyn HttpClient,
    deployment: &Deployment,
    path: &str,
    method: HttpMethod,
    token: Option<&str>,
    body: Option<Value>,
    query: Vec<(&str, String)>,
) -> PluginResult<(u16, Value)> {
    let mut url = url::Url::parse(&format!("{}{path}", deployment.openapi))
        .map_err(|_| invalid("The Qoder account address is invalid."))?;
    for (key, value) in query {
        url.query_pairs_mut().append_pair(key, &value);
    }
    let mut headers = BTreeMap::from([("accept".into(), "application/json".into())]);
    if let Some(token) = token {
        headers.insert("authorization".into(), format!("Bearer {token}"));
    }
    let body = body.map(|value| value.to_string().into_bytes());
    if body.is_some() {
        headers.insert("content-type".into(), "application/json".into());
    }
    let (status, _, bytes) = read(
        send(
            client,
            HttpRequest {
                method,
                url: url.into(),
                headers,
                body,
            },
            20,
        )
        .await?,
        4 * 1024 * 1024,
    )
    .await?;
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Ok((status, value))
}

#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;

#[cfg(test)]
#[path = "../test/qwenwork.test.rs"]
mod qwenwork_tests;

#[cfg(test)]
#[path = "../test/qwenwork_auth_methods.test.rs"]
mod qwenwork_auth_methods_tests;
