use super::{
    auth, models,
    network::{self, Client},
    request,
    session::{Open, Pool},
    stream, usage,
};
use async_trait::async_trait;
use nora_ai::ReqwestHttpClient;
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, PluginError,
    PluginResult, ProviderPlugin, API_VERSION,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::sync::Mutex;
#[async_trait]
pub(crate) trait Persistence: Send + Sync {
    async fn save(&self, value: Value) -> PluginResult<()>;
}
struct Host;
#[async_trait]
impl Persistence for Host {
    async fn save(&self, value: Value) -> PluginResult<()> {
        nora_provider_plugin_sdk::save_credentials(value).await
    }
}
#[derive(Clone)]
struct Config {
    proxy: Option<NetworkProxy>,
    methods: Value,
    version: String,
}
#[derive(Default)]
struct Rotations {
    pending: BTreeMap<String, Value>,
    after: BTreeMap<String, Value>,
}
pub(crate) struct Cursor {
    config: RwLock<Option<Config>>,
    client: Option<Client>,
    persistence: Arc<dyn Persistence>,
    pending: Mutex<BTreeMap<String, auth::Pending>>,
    rotations: Mutex<Rotations>,
    models: Mutex<BTreeMap<String, (u64, Vec<models::Entry>)>>,
    pool: Pool,
    max_only: Mutex<BTreeSet<String>>,
}
impl Default for Cursor {
    fn default() -> Self {
        Self {
            config: RwLock::new(None),
            client: None,
            persistence: Arc::new(Host),
            pending: Mutex::new(BTreeMap::new()),
            rotations: Mutex::new(Rotations::default()),
            models: Mutex::new(BTreeMap::new()),
            pool: Pool::default(),
            max_only: Mutex::new(BTreeSet::new()),
        }
    }
}
impl Cursor {
    fn config(&self) -> PluginResult<Config> {
        self.config
            .read()
            .map_err(|_| PluginError::invalid("Cursor configuration is unavailable."))?
            .clone()
            .ok_or_else(|| PluginError::invalid("Initialize Cursor before using it."))
    }
    fn proxy(&self, proxy: Option<&NetworkProxy>) -> PluginResult<Option<NetworkProxy>> {
        let proxy = proxy.cloned().or(self.config()?.proxy);
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        Ok(proxy)
    }
    fn client(&self, proxy: Option<&NetworkProxy>) -> PluginResult<Client> {
        let proxy = self.proxy(proxy)?;
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        Ok(Arc::new(
            ReqwestHttpClient::with_client(
                nora_provider_plugin_sdk::http_client_builder(proxy.as_ref())?
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .map_err(|_| {
                        PluginError::invalid("Cursor HTTP client could not be configured.")
                    })?,
            )
            .with_observer_fn(|| async {
                nora_provider_plugin_sdk::before_http_request()
                    .await
                    .map_err(|error| nora_ai::HttpClientError::Transport(error.to_string()))
            }),
        ))
    }
    fn params_client(&self, params: &Value) -> PluginResult<Client> {
        let proxy: Option<NetworkProxy> =
            serde_json::from_value(params.get("networkProxy").cloned().unwrap_or(Value::Null))
                .map_err(|_| PluginError::invalid("The Cursor network proxy is invalid."))?;
        self.client(proxy.as_ref())
    }
    fn transport_scope(&self, proxy: Option<&NetworkProxy>) -> PluginResult<String> {
        Ok(auth::hash(
            serde_json::to_vec(&self.proxy(proxy)?)
                .map_err(|_| PluginError::invalid("The Cursor network proxy is invalid."))?,
        ))
    }
    async fn live(
        &self,
        value: Option<&Value>,
        client: &dyn nora_ai::HttpClient,
        force: bool,
    ) -> PluginResult<Value> {
        let mut value = value
            .filter(|v| v.is_object())
            .cloned()
            .ok_or_else(auth::expired)?;
        let version = self.config()?.version;
        if let Some(source) = value.get("source") {
            let access = auth::source(source).await?;
            let expires = auth::expiry(&access);
            if expires > 0 && expires <= auth::now() {
                return Err(auth::expired());
            }
            let (email, plan) = network::identify(client, &access, &version).await?;
            if value["accountId"] != email {
                return Err(auth::expired());
            }
            value["access"] = json!(access);
            value["expires"] = json!(expires);
            value["plan"] = json!(plan);
            return Ok(value);
        }
        let key = auth::hash(value.to_string());
        let mut rotations = self.rotations.lock().await;
        if let Some(pending) = rotations.pending.get(&key).cloned() {
            self.persistence.save(pending.clone()).await?;
            rotations.pending.remove(&key);
            rotations.after.insert(key.clone(), pending);
        }
        for _ in 0..32 {
            let key = auth::hash(value.to_string());
            let Some(next) = rotations.after.get(&key).cloned() else {
                break;
            };
            value = next;
        }
        let access = auth::token(value.get("access"))?;
        let jwt_expiry = auth::expiry(&access);
        let exp = if jwt_expiry > 0 {
            jwt_expiry
        } else {
            value["expires"].as_u64().unwrap_or(0)
        };
        if value["authMethod"] == "apiKey" && (force || exp > 0 && exp <= auth::now() + 300_000) {
            let key = auth::hash(value.to_string());
            let api_key = auth::token(value.get("key"))?;
            let access = auth::exchange(client, &api_key, &version).await?;
            let (email, plan) = network::identify(client, &access, &version).await?;
            if value["accountId"] != email {
                return Err(auth::expired());
            }
            let mut next = value.clone();
            next["access"] = json!(access);
            next["expires"] = json!(auth::expiry(&access));
            next["plan"] = json!(plan);
            if rotations.after.len() >= 64 {
                rotations.after.clear();
            }
            rotations.pending.insert(key.clone(), next.clone());
            self.persistence.save(next.clone()).await?;
            rotations.pending.remove(&key);
            rotations.after.insert(key, next.clone());
            return Ok(next);
        }
        if exp > 0 && exp <= auth::now() {
            return Err(auth::expired());
        }
        if auth::text(value.get("accountId"), 512).is_none() {
            return Err(auth::expired());
        }
        Ok(value)
    }
    async fn listing(
        &self,
        client: &dyn nora_ai::HttpClient,
        credential: &Value,
        fresh: bool,
    ) -> PluginResult<Vec<models::Entry>> {
        let token = auth::token(credential.get("access"))?;
        let key = auth::hash(&token);
        if !fresh {
            if let Some((at, entries)) = self
                .models
                .lock()
                .await
                .get(&key)
                .filter(|(at, _)| auth::now().saturating_sub(*at) < 600_000)
            {
                let _ = at;
                return Ok(entries.clone());
            }
        }
        let entries = models::fetch(client, &token, &self.config()?.version).await?;
        let mut cache = self.models.lock().await;
        if cache.len() >= 32 {
            cache.clear();
        }
        cache.insert(key, (auth::now(), entries.clone()));
        Ok(entries)
    }
    async fn open(
        &self,
        request: &FetchRequest,
        chat: &Value,
        credential: &Value,
        client: Client,
    ) -> PluginResult<session::Guard> {
        let tools = request::tools(chat)?;
        let model = auth::text(chat.get("model"), 512)
            .ok_or_else(|| PluginError::invalid("The Cursor model ID is missing."))?;
        let entries = self.listing(client.as_ref(), credential, false).await?;
        let effort = request
            .metadata
            .get("reasoningEffort")
            .map(String::as_str)
            .or_else(|| chat["reasoning_effort"].as_str())
            .unwrap_or_default();
        let fast = chat["service_tier"]
            .as_str()
            .map(|s| s == "fast" || s == "priority");
        let mut selection = models::select(&entries, model, effort, fast);
        selection.max_mode |= self.max_only.lock().await.contains(model);
        let token = auth::token(credential.get("access"))?;
        let version = self.config()?.version;
        let proxy = self.proxy(request.network_proxy.as_ref())?;
        let h2_client = if self.client.is_some() {
            None
        } else {
            Some(
                nora_provider_plugin_sdk::http_client_builder(proxy.as_ref())?
                    .http2_prior_knowledge()
                    .redirect(reqwest::redirect::Policy::none())
                    .read_timeout(Duration::from_secs(125))
                    .build()
                    .map_err(|_| {
                        PluginError::invalid("Cursor HTTP/2 client could not be configured.")
                    })?,
            )
        };
        let base = network::agent_url(client.as_ref(), &token, &version).await;
        let account = auth::hash(format!(
            "{}\0{}\0{}",
            request.account,
            credential["accountId"],
            auth::hash(&token)
        ));
        let controls=auth::hash(json!({"account":account,"proxy":proxy,"model":chat["model"],"tools":chat["tools"],"choice":chat["tool_choice"],"effort":effort,"tier":chat["service_tier"],"selector":request::selector(chat,&request.metadata,&account)}).to_string());
        self.pool
            .acquire(
                Open {
                    client,
                    h2_client,
                    token,
                    version,
                    base,
                    account: account.clone(),
                    controls,
                    selector: request::selector(chat, &request.metadata, &account),
                    selection,
                    tools,
                    network_scope: self.transport_scope(request.network_proxy.as_ref())?,
                },
                chat,
            )
            .await
    }
}
use super::session;
#[async_trait]
impl ProviderPlugin for Cursor {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        if params.api_version != API_VERSION
            || params.plugin_id != "cursor"
            || params.manifest["id"] != "cursor"
            || params.manifest["baseURL"] != network::BASE
        {
            return Err(PluginError::invalid(
                "The Cursor executable does not match its manifest.",
            ));
        }
        let proxy: Option<NetworkProxy> = serde_json::from_value(
            params
                .manifest
                .pointer("/config/networkProxy")
                .cloned()
                .unwrap_or(Value::Null),
        )
        .map_err(|_| PluginError::invalid("The Cursor network proxy is invalid."))?;
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        let version = params
            .manifest
            .pointer("/config/clientVersion")
            .and_then(Value::as_str)
            .unwrap_or(network::FALLBACK_VERSION);
        if version.len() > 128
            || !version.starts_with("cli-")
            || !version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
        {
            return Err(PluginError::invalid(
                "The Cursor client version is invalid.",
            ));
        }
        *self
            .config
            .write()
            .map_err(|_| PluginError::invalid("Cursor configuration is unavailable."))? =
            Some(Config {
                proxy,
                methods: params.manifest["authMethods"].clone(),
                version: version.into(),
            });
        Ok(json!({"apiVersion":API_VERSION}))
    }
    async fn auth_methods(&self, _: CallContext) -> PluginResult<Value> {
        Ok(self.config()?.methods)
    }
    async fn accounts(&self, context: CallContext) -> PluginResult<Value> {
        let Some(c) = context.credentials else {
            return Ok(json!([]));
        };
        let account = auth::text(c.get("accountId"), 512).ok_or_else(auth::expired)?;
        Ok(json!([{"id":context.account,"name":account,"email":account,"plan":c["plan"]}]))
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        let client = self.params_client(&params)?;
        let version = self.config()?.version;
        let method = params
            .get("authMethodID")
            .or_else(|| params.get("method"))
            .and_then(Value::as_str)
            .unwrap_or("browser");
        let inputs = params
            .get("inputs")
            .or_else(|| params.get("fields"))
            .unwrap_or(&params);
        if method == "browser" {
            let (pending, url) = auth::start(client, version);
            let session = auth::random();
            let mut sessions = self.pending.lock().await;
            sessions.retain(|_, p| auth::now().saturating_sub(p.created) < 600_000);
            if sessions.len() >= 8 {
                return Err(PluginError::invalid(
                    "Finish an existing Cursor sign-in first.",
                ));
            }
            sessions.insert(session.clone(), pending);
            return Ok(
                json!({"session":session,"url":url,"method":"auto","instructions":"Sign in to Cursor in your browser."}),
            );
        }
        let (access, source, key) = match method {
            "apiKey" => {
                let key = auth::token(inputs.get("apiKey").or_else(|| inputs.get("key")))?;
                (
                    auth::exchange(client.as_ref(), &key, &version).await?,
                    None,
                    Some(key),
                )
            }
            "cliSource" => {
                let path = auth::text(inputs.get("sourcePath"), 16 * 1024).ok_or_else(|| {
                    PluginError::invalid("Select one cursor-agent auth.json file.")
                })?;
                let source = json!({"kind":"file","path":path});
                (auth::source(&source).await?, Some(source), None)
            }
            "keychainSource" => {
                let source = json!({"kind":"keychain"});
                (auth::source(&source).await?, Some(source), None)
            }
            _ => return Err(PluginError::unsupported(method)),
        };
        let expires = auth::expiry(&access);
        if expires > 0 && expires <= auth::now() {
            return Err(auth::expired());
        }
        let (email, plan) = network::identify(client.as_ref(), &access, &version).await?;
        let mut credentials = json!({"type":if method=="apiKey"{"api"}else{"oauth"},"authMethod":method,"accountId":email,"email":email,"label":email,"plan":plan});
        if let Some(source) = source {
            credentials["source"] = source;
        } else {
            credentials["access"] = json!(access);
            credentials["expires"] = json!(expires);
            credentials["key"] = json!(key);
        }
        Ok(json!({"ok":true,"credentials":credentials}))
    }
    async fn callback(&self, params: Value) -> PluginResult<Value> {
        let session = auth::text(params.get("session"), 256).ok_or_else(auth::expired)?;
        let pending = self
            .pending
            .lock()
            .await
            .remove(session)
            .ok_or_else(auth::expired)?;
        let credentials = pending.complete().await?;
        Ok(json!({"ok":true,"credentials":credentials}))
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let client = self.params_client(&params)?;
        self.live(
            params.get("credentials"),
            client.as_ref(),
            params["force"] == true,
        )
        .await?;
        Ok(json!({"changed":false}))
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        if context.credentials.is_none() {
            return Ok(
                json!({"models":[],"source":"preset","allowUnknownModels":true,"defaultAPI":"openai-completions","defaultBaseURL":network::BASE}),
            );
        }
        let client = self.client(context.network_proxy.as_ref())?;
        let c = self
            .live(context.credentials.as_ref(), client.as_ref(), false)
            .await?;
        Ok(models::output(
            &self.listing(client.as_ref(), &c, true).await?,
        ))
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let client = self.client(context.network_proxy.as_ref())?;
        let c = self
            .live(context.credentials.as_ref(), client.as_ref(), false)
            .await?;
        let entries = self.listing(client.as_ref(), &c, false).await?;
        let mut value = usage::fetch(
            client.as_ref(),
            &auth::token(c.get("access"))?,
            &self.config()?.version,
            &entries.iter().map(|e| e.id.clone()).collect::<Vec<_>>(),
        )
        .await?;
        value["account"] = json!({"id":context.account,"email":c["accountId"],"plan":c["plan"]});
        if let Some(plan) = c["plan"].as_str() {
            value["plan"] = json!(plan);
        }
        nora_provider::usage::parsing::update_availability(&mut value);
        Ok(value)
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        if request.method != "POST" || request.url != format!("{}/chat/completions", network::BASE)
        {
            return Err(PluginError::invalid(
                "Cursor accepts chat requests only at its declared local protocol endpoint.",
            ));
        }
        let bytes = request
            .decode_body()?
            .ok_or_else(|| PluginError::invalid("The Cursor request body is missing."))?;
        if bytes.len() > 32 * 1024 * 1024 {
            return Err(PluginError::invalid("The Cursor request is too large."));
        }
        let chat: Value = serde_json::from_slice(&bytes)
            .map_err(|_| PluginError::invalid("The Cursor request is invalid JSON."))?;
        for field in [
            "temperature",
            "top_p",
            "stop",
            "response_format",
            "logprobs",
            "top_logprobs",
        ] {
            if chat.get(field).is_some_and(|v| !v.is_null()) {
                return Err(PluginError::invalid(format!(
                    "Cursor Agent does not support {field}."
                )));
            }
        }
        let client = self.client(request.network_proxy.as_ref())?;
        let mut credential = self
            .live(request.credentials.as_ref(), client.as_ref(), false)
            .await?;
        let mut renewed = false;
        let mut max_retried = false;
        let mut region_retried = false;
        loop {
            let result = match self
                .open(&request, &chat, &credential, client.clone())
                .await
            {
                Ok(run) => stream::response(run, &chat).await,
                Err(error) => Err(error),
            };
            match result {
                Ok(response) => return Ok(response),
                Err(error)
                    if error.code == "authentication_required"
                        && credential["authMethod"] == "apiKey"
                        && !renewed =>
                {
                    renewed = true;
                    credential = self
                        .live(request.credentials.as_ref(), client.as_ref(), true)
                        .await?;
                }
                Err(error) if error.code == "cursor_max_mode_required" && !max_retried => {
                    max_retried = true;
                    let model = auth::text(chat.get("model"), 512)
                        .ok_or_else(|| PluginError::invalid("The Cursor model ID is missing."))?;
                    self.max_only.lock().await.insert(model.to_owned());
                }
                Err(error) if error.code == "cursor_region_required" && !region_retried => {
                    region_retried = true;
                    self.pool
                        .reset_transport(&self.transport_scope(request.network_proxy.as_ref())?);
                }
                Err(error) => return Ok(stream::failure(error)),
            }
        }
    }

    async fn call(&self, method: &str, _: Value) -> PluginResult<Value> {
        if method == "maintain" {
            return Ok(json!({"ok":true,"modelsChanged":false}));
        }
        Err(PluginError::unsupported(method))
    }
}

#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
