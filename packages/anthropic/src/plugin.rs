use super::{
    cli, cli_usage, login,
    provider::Anthropic,
    proxy::Proxy,
    session::Pool,
    stream::{self, Host, Persistence},
};
use async_trait::async_trait;
use nora_provider::{auth::is_api, BundledProvider};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, PluginError,
    PluginResult, ProviderPlugin,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, RwLock},
};
use tokio::sync::Mutex;
#[derive(Clone)]
struct Config {
    root: PathBuf,
    proxy: Option<NetworkProxy>,
}
pub(crate) struct Claude {
    inner: BundledProvider,
    config: RwLock<Option<Config>>,
    logins: Mutex<BTreeMap<String, login::Pending>>,
    pool: Pool,
    proxies: Mutex<BTreeMap<String, Arc<Proxy>>>,
    pub persistence: Arc<dyn Persistence>,
    usage: Mutex<BTreeMap<String, Value>>,
}
impl Default for Claude {
    fn default() -> Self {
        Self {
            inner: BundledProvider::new(Anthropic),
            config: RwLock::new(None),
            logins: Mutex::new(BTreeMap::new()),
            pool: Pool::default(),
            proxies: Mutex::new(BTreeMap::new()),
            persistence: Arc::new(Host),
            usage: Mutex::new(BTreeMap::new()),
        }
    }
}
impl Claude {
    fn config(&self) -> PluginResult<Config> {
        self.config
            .read()
            .map_err(|_| cli::error())?
            .clone()
            .ok_or_else(|| PluginError::invalid("Initialize Anthropic before using it."))
    }
    async fn proxy(&self, proxy: Option<&NetworkProxy>) -> PluginResult<Arc<Proxy>> {
        let config = self.config()?;
        let selected = proxy.or(config.proxy.as_ref());
        if let Some(selected) = selected {
            selected.validate()?;
        }
        let key = cli::hash(&serde_json::to_string(&selected).map_err(|_| cli::error())?);
        let mut proxies = self.proxies.lock().await;
        if let Some(proxy) = proxies.get(&key) {
            return Ok(proxy.clone());
        }
        let proxy = Proxy::new(selected).await?;
        if proxies.len() >= 16 {
            proxies.clear();
        }
        proxies.insert(key, proxy.clone());
        Ok(proxy)
    }
    async fn params_proxy(&self, params: &Value) -> PluginResult<Arc<Proxy>> {
        let proxy: Option<NetworkProxy> =
            serde_json::from_value(params.get("networkProxy").cloned().unwrap_or(Value::Null))
                .map_err(|_| PluginError::invalid("The Claude network proxy is invalid."))?;
        self.proxy(proxy.as_ref()).await
    }
    async fn account(
        &self,
        id: &str,
        value: &Value,
        proxy: Arc<Proxy>,
    ) -> PluginResult<cli::Account> {
        let root = self.config()?.root;
        let (executable, directory, expected) = cli::materialize(&root, id, value)?;
        let mut account =
            cli::identify(&executable, &directory, proxy, expected.as_deref()).await?;
        let expected_email = cli::text(
            value
                .get("email")
                .or_else(|| value.pointer("/extras/email")),
            512,
        );
        if expected.is_none()
            && expected_email.is_some_and(|email| !email.eq_ignore_ascii_case(&account.email))
        {
            return Err(cli::error());
        }
        let previous = account.saved.clone();
        let next = account.saved();
        let changed = value.pointer("/extras/noraClaudeCLI").is_none()
            || value["access"] != previous["access"]
            || value["refresh"] != previous["refresh"];
        if changed {
            self.persistence.save(next).await?;
        }
        account.saved = previous;
        Ok(account)
    }
}
#[async_trait]
impl ProviderPlugin for Claude {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        let root = params.data_directory.join("claude-accounts");
        let proxy: Option<NetworkProxy> = serde_json::from_value(
            params
                .manifest
                .pointer("/config/networkProxy")
                .cloned()
                .unwrap_or(Value::Null),
        )
        .map_err(|_| PluginError::invalid("The Claude network proxy is invalid."))?;
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        let result = self.inner.initialize(params).await?;
        cli::directory(&root)?;
        *self.config.write().map_err(|_| cli::error())? = Some(Config { root, proxy });
        Ok(result)
    }
    async fn auth_methods(&self, context: CallContext) -> PluginResult<Value> {
        self.inner.auth_methods(context).await
    }
    async fn accounts(&self, context: CallContext) -> PluginResult<Value> {
        let Some(value) = context.credentials.as_ref() else {
            return Ok(json!([]));
        };
        if is_api(value) {
            return self.inner.accounts(context).await;
        }
        let mut account = json!({"id":context.account});
        for key in ["email", "plan", "accountId", "label"] {
            if let Some(text) = cli::text(value.get(key), 512) {
                account[key] = json!(text);
            }
        }
        Ok(json!([account]))
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        let method = params
            .get("authMethodID")
            .or_else(|| params.get("method"))
            .and_then(Value::as_str)
            .unwrap_or("oauth");
        if method != "oauth" {
            return self.inner.authorize(params).await;
        }
        let inputs = params
            .get("inputs")
            .or_else(|| params.get("fields"))
            .unwrap_or(&params);
        let selected = cli::text(inputs.get("claudePath"), 16 * 1024);
        let proxy = self.params_proxy(&params).await?;
        let (pending, url) = login::start(&self.config()?.root, selected, proxy).await?;
        let session = cli::random();
        let mut logins = self.logins.lock().await;
        if logins.len() >= 8 {
            logins.clear();
        }
        logins.insert(session.clone(), pending);
        Ok(
            json!({"session":session,"url":url,"method":"auto","instructions":"Sign in with the installed Claude Code. Paste the returned code if the browser cannot finish automatically."}),
        )
    }
    async fn callback(&self, params: Value) -> PluginResult<Value> {
        let id = cli::text(params.get("session"), 256).ok_or_else(cli::error)?;
        let pending = self.logins.lock().await.remove(id).ok_or_else(cli::error)?;
        let account = pending.finish(&params).await?;
        Ok(json!({"ok":true,"credentials":account.saved()}))
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let value = params.get("credentials").ok_or_else(cli::error)?;
        if is_api(value) {
            return self.inner.refresh(params).await;
        }
        let proxy = self.params_proxy(&params).await?;
        let account = params
            .get("account")
            .and_then(Value::as_str)
            .unwrap_or("default");
        self.account(account, value, proxy).await?;
        Ok(json!({"changed":false}))
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        self.inner.models(context).await
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let value = context.credentials.as_ref().ok_or_else(cli::error)?;
        if is_api(value) {
            return self.inner.usage(context).await;
        }
        let proxy = self.proxy(context.network_proxy.as_ref()).await?;
        let mut account = self.account(&context.account, value, proxy.clone()).await?;
        let bytes = cli::output(
            &account.executable,
            &[
                "-p",
                "/usage",
                "--output-format",
                "json",
                "--tools",
                "",
                "--strict-mcp-config",
                "--setting-sources",
                "",
                "--no-session-persistence",
            ],
            &account.directory,
            proxy,
            true,
        )
        .await?;
        let payload: Value = serde_json::from_slice(&bytes).map_err(|_| {
            PluginError::transport("Claude Code's usage command returned invalid JSON.")
        })?;
        if payload["is_error"] == true {
            if stream::status(&payload) == 401 {
                return Err(cli::error());
            }
            return Err(PluginError::transport(
                "Claude Code could not read its subscription allowances.",
            ));
        }
        stream::synchronize(&mut account, self.persistence.as_ref()).await?;
        let parsed = cli_usage::parse(
            payload["result"].as_str().unwrap_or_default(),
            chrono::Utc::now(),
        );
        let mut cache = self.usage.lock().await;
        let mut output = match parsed {
            Ok(value) => {
                if cache.len() >= 32 {
                    cache.clear();
                }
                cache.insert(account.identity.clone(), value.clone());
                value
            }
            Err(error) => {
                if error.code == "authentication_required" {
                    return Err(error);
                }
                let Some(previous) = cache.get(&account.identity) else {
                    return Err(error);
                };
                let mut previous = previous.clone();
                previous["stale"] = json!(true);
                previous
            }
        };
        output["account"] = json!({"id":context.account,"email":account.email,"plan":account.plan});
        output["plan"] = json!(account.plan);
        Ok(output)
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        let value = request.credentials.as_ref().ok_or_else(cli::error)?;
        if is_api(value) {
            return self.inner.fetch(request).await;
        }
        let url = url::Url::parse(&request.url)
            .map_err(|_| PluginError::invalid("The Claude request address is invalid."))?;
        if request.method != "POST"
            || url.scheme() != "https"
            || url.host_str() != Some("api.anthropic.com")
            || url.port_or_known_default() != Some(443)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || !["/v1/messages", "/messages"].contains(&url.path())
            || url
                .query_pairs()
                .any(|(key, value)| key != "beta" || value != "true")
        {
            return Err(PluginError::invalid(
                "Claude subscriptions accept Anthropic Messages at the official endpoint.",
            ));
        }
        let credentials = value.clone();
        let proxy = self.proxy(request.network_proxy.as_ref()).await?;
        let metadata = request.metadata.clone();
        let account_id = request.account.clone();
        let mut body = tokio::task::spawn_blocking(move || {
            let bytes = request
                .decode_body()?
                .ok_or_else(|| PluginError::invalid("The Claude request body is missing."))?;
            serde_json::from_slice::<Value>(&bytes)
                .map_err(|_| PluginError::invalid("The Claude request body is invalid JSON."))
        })
        .await
        .map_err(|_| PluginError::invalid("The Claude request body could not be read."))??;
        if let Some(effort) = metadata.get("reasoningEffort").filter(|effort| {
            [
                "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
            ]
            .contains(&effort.as_str())
        }) {
            if effort == "none" {
                body["thinking"] = json!({"type":"disabled"});
            } else {
                if !body["output_config"].is_object() {
                    body["output_config"] = json!({});
                }
                body["output_config"]["effort"] = json!(match effort.as_str() {
                    "minimal" => "low",
                    "ultra" => "max",
                    other => other,
                });
            }
        }
        let account = self
            .account(&account_id, &credentials, proxy.clone())
            .await?;
        let streaming = body["stream"] == true;
        let run = self.pool.acquire(account, &body, &metadata, proxy).await?;
        stream::response(run, streaming, self.persistence.clone()).await
    }
}
#[cfg(test)]
#[path = "../test/plugin.test.rs"]
mod tests;
