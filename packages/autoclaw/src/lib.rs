//! AutoClaw account protocol, adapted from magpie-community/plugins (MIT).
//! Login, rotation, chat and credits are native supplier implementations.

mod auth;
mod chat;
mod guide;
mod models;
mod refusal;
mod transport;
mod usage;

use async_trait::async_trait;
use nora_ai::{HttpClient, HttpMethod, HttpRequest, HttpStreamingResponse, ReqwestHttpClient};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, PluginError,
    PluginResult, ProviderPlugin, API_VERSION,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Site {
    International,
    China,
}
impl Site {
    pub fn id(self) -> &'static str {
        match self {
            Self::International => "autoclaw",
            Self::China => "autoclaw-cn",
        }
    }
    fn host(self) -> &'static str {
        match self {
            Self::International => "https://autoglm-api.autoglm.ai",
            Self::China => "https://autoglm-api.zhipuai.cn",
        }
    }
    fn other(self) -> Self {
        match self {
            Self::International => Self::China,
            Self::China => Self::International,
        }
    }
}

#[derive(Clone)]
struct Config {
    proxy: Option<NetworkProxy>,
    methods: Value,
    subscriptions_enabled: bool,
}

#[async_trait]
trait CredentialSink: Send + Sync {
    async fn save(&self, credentials: Value) -> PluginResult<()>;
}
struct HostSink;
#[async_trait]
impl CredentialSink for HostSink {
    async fn save(&self, credentials: Value) -> PluginResult<()> {
        nora_provider_plugin_sdk::save_credentials(credentials).await
    }
}

pub struct Autoclaw {
    site: Site,
    config: RwLock<Option<Config>>,
    pending: Mutex<BTreeMap<String, auth::Pending>>,
    client: Option<Arc<dyn HttpClient>>,
    sink: Arc<dyn CredentialSink>,
}
impl Autoclaw {
    pub fn new(site: Site) -> Self {
        Self {
            site,
            config: RwLock::new(None),
            pending: Mutex::new(BTreeMap::new()),
            client: None,
            sink: Arc::new(HostSink),
        }
    }
    fn config(&self) -> PluginResult<Config> {
        self.config
            .read()
            .map_err(|_| invalid("AutoClaw settings are unavailable."))?
            .clone()
            .ok_or_else(|| invalid("Initialize AutoClaw before using it."))
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
        self.require_subscriptions(
            params
                .get("subscriptionAccountsEnabled")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        )?;
        let proxy: Option<NetworkProxy> =
            serde_json::from_value(params.get("networkProxy").cloned().unwrap_or(Value::Null))
                .map_err(|_| invalid("AutoClaw account network settings are invalid."))?;
        self.client(proxy.as_ref())
    }
    fn require_subscriptions(&self, enabled: bool) -> PluginResult<()> {
        nora_provider_plugin_sdk::subscription_policy::require_subscription_accounts()?;
        if !enabled || !self.config()?.subscriptions_enabled {
            return Err(PluginError::new(
                "subscription_accounts_disabled",
                "Subscription accounts are disabled.",
            ));
        }
        Ok(())
    }
    fn credentials(&self, credentials: &Value) -> PluginResult<()> {
        if credentials["type"] != "oauth"
            || auth::bare(text(credentials, "access")).is_empty()
            || credentials
                .get("provider")
                .and_then(Value::as_str)
                .is_some_and(|id| id != self.site.id())
        {
            return Err(signed_out());
        }
        Ok(())
    }
    async fn current(&self, client: &dyn HttpClient, credentials: &Value) -> PluginResult<Value> {
        self.credentials(credentials)?;
        if credentials["expires"]
            .as_u64()
            .is_some_and(|at| at <= now())
        {
            let next = auth::renew(client, self.site, credentials).await?;
            // AutoClaw rotates refresh tokens. Persist the pair before any
            // profile, wallet, model or chat request can use the new access.
            self.sink.save(next.clone()).await?;
            return Ok(next);
        }
        Ok(credentials.clone())
    }
}

#[async_trait]
impl ProviderPlugin for Autoclaw {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        if params.api_version != API_VERSION
            || params.plugin_id != self.site.id()
            || params.manifest["id"] != self.site.id()
        {
            return Err(invalid(
                "The AutoClaw executable and provider manifest do not match.",
            ));
        }
        let proxy: Option<NetworkProxy> = serde_json::from_value(
            params
                .manifest
                .pointer("/config/networkProxy")
                .cloned()
                .unwrap_or(Value::Null),
        )
        .map_err(|_| invalid("AutoClaw network settings are invalid."))?;
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        *self
            .config
            .write()
            .map_err(|_| invalid("AutoClaw settings are unavailable."))? = Some(Config {
            proxy,
            methods: params
                .manifest
                .get("authMethods")
                .cloned()
                .unwrap_or(json!([])),
            subscriptions_enabled: params.subscription_accounts_enabled,
        });
        Ok(
            json!({"apiVersion":API_VERSION,"capabilities":{"persistent":true,"sdkFeatures":{"accountAutomation":1}}}),
        )
    }
    async fn auth_methods(&self, _: CallContext) -> PluginResult<Value> {
        Ok(self.config()?.methods)
    }
    async fn accounts(&self, context: CallContext) -> PluginResult<Value> {
        let Some(mut credentials) = context.credentials else {
            return Ok(json!([]));
        };
        self.credentials(&credentials)?;
        let changed = auth::identity(self.site, &mut credentials)?;
        let mut result = json!({"accounts":[{"id":context.account,"name":credentials.get("name"),"email":credentials.get("email"),"configured":true}]});
        if changed {
            result["credentials"] = credentials;
        }
        Ok(result)
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        auth::authorize(self, params).await
    }
    async fn callback(&self, params: Value) -> PluginResult<Value> {
        auth::callback(self, params).await
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let credentials = params.get("credentials").ok_or_else(signed_out)?;
        self.credentials(credentials)?;
        let next = auth::renew(
            self.params_client(&params)?.as_ref(),
            self.site,
            credentials,
        )
        .await?;
        self.sink.save(next.clone()).await?;
        Ok(json!({"credentials":next}))
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        let Some(credentials) = context.credentials else {
            return Ok(json!({"models":[],"source":"catalog","allowUnknownModels":false}));
        };
        self.require_subscriptions(context.subscription_accounts_enabled)?;
        self.credentials(&credentials)?;
        Ok(json!({"models":models::list(self.site),"source":"catalog","allowUnknownModels":false}))
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        self.require_subscriptions(context.subscription_accounts_enabled)?;
        let client = self.client(context.network_proxy.as_ref())?;
        let credentials = self
            .current(
                client.as_ref(),
                context.credentials.as_ref().ok_or_else(signed_out)?,
            )
            .await?;
        usage::fetch(client.as_ref(), self.site, &credentials).await
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        chat::fetch(self, request).await
    }
    async fn call(&self, method: &str, params: Value) -> PluginResult<Value> {
        if method == "cancelAuthorization" {
            return auth::cancel(self, &params).await;
        }
        if !matches!(method, "checkin" | "checkinStatus") {
            return Err(PluginError::unsupported(method));
        }
        let client = self.params_client(&params)?;
        let credentials = self
            .current(
                client.as_ref(),
                params.get("credentials").ok_or_else(signed_out)?,
            )
            .await?;
        usage::checkin(
            client.as_ref(),
            self.site,
            &credentials,
            method == "checkinStatus",
        )
        .await
    }
}

fn invalid(message: &str) -> PluginError {
    PluginError::invalid(message)
}
fn signed_out() -> PluginError {
    PluginError::authentication("Sign in to this AutoClaw account again.")
}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|n| n.is_finite())
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as u64
}
fn random_hex(count: usize) -> PluginResult<String> {
    let mut bytes = vec![0u8; count];
    getrandom::getrandom(&mut bytes)
        .map_err(|_| invalid("The AutoClaw sign-in random source is unavailable."))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;

#[cfg(test)]
#[path = "../test/compiled.test.rs"]
mod compiled_tests;

#[cfg(test)]
#[path = "../test/refusal.test.rs"]
mod refusal_tests;

#[cfg(test)]
#[path = "../test/refusal-protocol.test.rs"]
mod refusal_protocol_tests;
