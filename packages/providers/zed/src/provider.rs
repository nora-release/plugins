use super::{
    account::{self, Account},
    auth, models,
    network::{self, Client},
    request, stream,
};
use async_trait::async_trait;
use nora_ai::{HttpMethod, ReqwestHttpClient};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, PluginError,
    PluginResult, ProviderPlugin, API_VERSION,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
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
}
#[derive(Default)]
struct Rotations {
    pending: BTreeMap<String, Value>,
    after: BTreeMap<String, Value>,
}
pub(crate) struct Zed {
    config: RwLock<Option<Config>>,
    client: Option<Client>,
    persistence: Arc<dyn Persistence>,
    pending: Mutex<BTreeMap<String, (Instant, auth::Pending)>>,
    tokens: Mutex<BTreeMap<String, String>>,
    models: Mutex<BTreeMap<String, (Instant, Vec<models::Model>)>>,
    rotations: Mutex<Rotations>,
}
impl Default for Zed {
    fn default() -> Self {
        Self {
            config: RwLock::new(None),
            client: None,
            persistence: Arc::new(Host),
            pending: Mutex::new(BTreeMap::new()),
            tokens: Mutex::new(BTreeMap::new()),
            models: Mutex::new(BTreeMap::new()),
            rotations: Mutex::new(Rotations::default()),
        }
    }
}
impl Zed {
    fn config(&self) -> PluginResult<Config> {
        self.config
            .read()
            .map_err(|_| PluginError::invalid("Zed configuration is unavailable."))?
            .clone()
            .ok_or_else(|| PluginError::invalid("Initialize Zed before using it."))
    }
    fn client(&self, proxy: Option<&NetworkProxy>) -> PluginResult<Client> {
        let proxy = proxy.cloned().or(self.config()?.proxy);
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        let client = nora_provider_plugin_sdk::http_client_builder(proxy.as_ref())?
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| PluginError::invalid("The Zed HTTP client could not be configured."))?;
        Ok(Arc::new(
            ReqwestHttpClient::with_client(client).with_observer_fn(|| async {
                nora_provider_plugin_sdk::before_http_request()
                    .await
                    .map_err(|error| nora_ai::HttpClientError::Transport(error.to_string()))
            }),
        ))
    }
    fn params_client(&self, params: &Value) -> PluginResult<Client> {
        let proxy: Option<NetworkProxy> =
            serde_json::from_value(params.get("networkProxy").cloned().unwrap_or(Value::Null))
                .map_err(|_| PluginError::invalid("The Zed network proxy is invalid."))?;
        self.client(proxy.as_ref())
    }
    async fn live(&self, value: Option<&Value>) -> PluginResult<Account> {
        let mut value = value.cloned().ok_or_else(account::expired)?;
        let mut rotations = self.rotations.lock().await;
        let key = account::hash(&value.to_string());
        if let Some(next) = rotations.pending.get(&key).cloned() {
            self.persistence.save(next.clone()).await?;
            rotations.pending.remove(&key);
            rotations.after.insert(key, next);
        }
        for _ in 0..32 {
            let key = account::hash(&value.to_string());
            if let Some(next) = rotations.after.get(&key) {
                value = next.clone();
            } else {
                break;
            }
        }
        Account::parse(&value)
    }
    async fn cloud(
        &self,
        client: &dyn nora_ai::HttpClient,
        account: &Account,
        method: HttpMethod,
        path: &str,
        body: Option<Value>,
    ) -> PluginResult<Value> {
        let mut request =
            network::request(method, path).with_header("authorization", account.authorization());
        if !account.system.is_empty() {
            request = request.with_header("x-zed-system-id", &account.system);
        }
        if let Some(body) = body {
            request.body = Some(
                serde_json::to_vec(&body)
                    .map_err(|_| PluginError::invalid("The Zed request is invalid."))?,
            );
        }
        network::read(network::send(client, request).await?).await
    }
    async fn me(&self, client: &dyn nora_ai::HttpClient, account: &Account) -> PluginResult<Value> {
        let me = self
            .cloud(client, account, HttpMethod::Get, "/client/users/me", None)
            .await?;
        if !me.is_object() {
            return Err(PluginError::transport("Zed returned an invalid account."));
        }
        Ok(me)
    }
    async fn rotate(&self, old: &Account, next: &Account) -> PluginResult<()> {
        let key = account::hash(&old.saved.to_string());
        let value = next.credentials();
        let mut rotations = self.rotations.lock().await;
        if rotations.after.len() >= 64 {
            rotations.after.clear();
        }
        rotations.pending.insert(key.clone(), value.clone());
        self.persistence.save(value.clone()).await?;
        rotations.pending.remove(&key);
        rotations.after.insert(key, value);
        Ok(())
    }
    async fn repick(
        &self,
        client: &dyn nora_ai::HttpClient,
        account: &mut Account,
    ) -> PluginResult<bool> {
        let me = self.me(client, account).await?;
        self.pick(account, &me).await
    }
    async fn pick(&self, account: &mut Account, me: &Value) -> PluginResult<bool> {
        let org = account::org(me, &account.org);
        let plan = account::plan(me, &org);
        let label = account::who(me);
        if account.org == org
            && account.plan == plan
            && (label.is_empty() || label == account.label)
        {
            return Ok(false);
        }
        let mut next = account.clone();
        next.org = org;
        next.plan = plan;
        if !label.is_empty() {
            next.label = label;
        }
        self.rotate(account, &next).await?;
        next.saved = next.credentials();
        let moved = next.org != account.org;
        *account = next;
        Ok(moved)
    }
    async fn token(
        &self,
        client: &dyn nora_ai::HttpClient,
        account: &Account,
        renew: bool,
    ) -> PluginResult<String> {
        let key = account.token_key();
        let mut tokens = self.tokens.lock().await;
        if !renew {
            if let Some(token) = tokens.get(&key) {
                return Ok(token.clone());
            }
        }
        tokens.remove(&key);
        let value = self
            .cloud(
                client,
                account,
                HttpMethod::Post,
                "/client/llm_tokens",
                Some(json!({"organization_id":account.org})),
            )
            .await?;
        let token = account::text(value.get("token"), 64 * 1024)
            .filter(|s| !s.chars().any(char::is_whitespace))
            .ok_or_else(|| PluginError::transport("Zed returned no model token."))?
            .to_owned();
        if tokens.len() >= 32 {
            tokens.clear();
        }
        tokens.insert(key, token.clone());
        Ok(token)
    }
    async fn listing(
        &self,
        client: &dyn nora_ai::HttpClient,
        account: &mut Account,
        fresh: bool,
    ) -> PluginResult<Vec<models::Model>> {
        if !fresh {
            if let Some((_, rows)) = self
                .models
                .lock()
                .await
                .get(&account.token_key())
                .filter(|(at, _)| at.elapsed() < Duration::from_secs(600))
            {
                return Ok(rows.clone());
            }
        }
        let mut renewed = false;
        let mut moved = false;
        loop {
            let token = match self.token(client, account, renewed).await {
                Ok(token) => token,
                Err(e) if e.code == "zed_http_402" && !moved => {
                    moved = true;
                    if self.repick(client, account).await? {
                        renewed = false;
                        continue;
                    }
                    return Err(e);
                }
                Err(e) => return Err(e),
            };
            let response = network::send(
                client,
                network::request(HttpMethod::Get, "/models")
                    .with_header("authorization", format!("Bearer {token}"))
                    .with_header("x-zed-client-supports-x-ai", "true"),
            )
            .await?;
            if !renewed && network::stale(&response) {
                renewed = true;
                continue;
            }
            let status = response.status;
            let parsed = if (200..300).contains(&status) {
                models::parse(&network::read(response).await?)
            } else if status == 401 {
                Err(PluginError::new(
                    "zed_http_401",
                    "Zed refused the renewed model token (401). The account sign-in was kept.",
                ))
            } else {
                Err(network::failure(status))
            };
            match parsed {
                Ok(rows) => {
                    let mut cache = self.models.lock().await;
                    if cache.len() >= 32 {
                        cache.clear();
                    }
                    cache.insert(account.token_key(), (Instant::now(), rows.clone()));
                    return Ok(rows);
                }
                Err(e) if (e.code == "zed_http_402" || e.code == "zed_no_models") && !moved => {
                    moved = true;
                    if self.repick(client, account).await? {
                        renewed = false;
                        continue;
                    }
                    return Err(e);
                }
                Err(e) => return Err(e),
            }
        }
    }
    async fn finish_sign_in(
        &self,
        client: &dyn nora_ai::HttpClient,
        mut account: Account,
    ) -> PluginResult<Value> {
        let me = self.me(client, &account).await?;
        account.org = account::org(&me, "");
        account.plan = account::plan(&me, &account.org);
        account.label = account::who(&me);
        if account.label.is_empty() {
            return Err(PluginError::authentication(
                "Zed did not identify this account.",
            ));
        }
        if account::disabled(&me, &account.org) {
            return Err(PluginError::authentication(
                "This Zed organization has disabled hosted models.",
            ));
        }
        Ok(account.credentials())
    }
}
fn status(error: &PluginError) -> u16 {
    if error.code == "authentication_required" {
        401
    } else if error.code == "invalid_request" {
        400
    } else if error.code == "zed_no_models" {
        402
    } else {
        error
            .code
            .strip_prefix("zed_http_")
            .and_then(|s| s.parse::<u16>().ok())
            .filter(|n| (400..600).contains(n))
            .unwrap_or(502)
    }
}
#[async_trait]
impl ProviderPlugin for Zed {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        if params.api_version != API_VERSION
            || params.plugin_id != "zed"
            || params.manifest["id"] != "zed"
            || params.manifest["contributes"]["provider"]["baseURL"] != network::BASE
        {
            return Err(PluginError::invalid(
                "The Zed executable does not match its manifest.",
            ));
        }
        let proxy: Option<NetworkProxy> = serde_json::from_value(
            params
                .manifest
                .pointer("/contributes/provider/config/networkProxy")
                .cloned()
                .unwrap_or(Value::Null),
        )
        .map_err(|_| PluginError::invalid("The Zed network proxy is invalid."))?;
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        *self
            .config
            .write()
            .map_err(|_| PluginError::invalid("Zed configuration is unavailable."))? =
            Some(Config {
                proxy,
                methods: params.manifest["contributes"]["provider"]["authMethods"].clone(),
            });
        Ok(json!({"apiVersion":API_VERSION}))
    }
    async fn auth_methods(&self, _: CallContext) -> PluginResult<Value> {
        Ok(self.config()?.methods)
    }
    async fn accounts(&self, context: CallContext) -> PluginResult<Value> {
        let Some(value) = context.credentials else {
            return Ok(json!([]));
        };
        let account = Account::parse(&value)?;
        Ok(
            json!([{"id":context.account,"name":account.label,"plan":account::plan_name(&account.plan)}]),
        )
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        let method = params
            .get("authMethodID")
            .or_else(|| params.get("method"))
            .and_then(Value::as_str)
            .unwrap_or("browser");
        if method != "browser" {
            return Err(PluginError::unsupported(method));
        }
        let client = self.params_client(&params)?;
        let (pending, url) = auth::start(client).await?;
        let session = account::random();
        let mut sessions = self.pending.lock().await;
        sessions.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(600));
        if sessions.len() >= 8 {
            return Err(PluginError::invalid(
                "Finish an existing Zed sign-in first.",
            ));
        }
        sessions.insert(session.clone(), (Instant::now(), pending));
        Ok(
            json!({"session":session,"url":url,"method":"auto","instructions":"Sign in to Zed in your browser."}),
        )
    }
    async fn callback(&self, params: Value) -> PluginResult<Value> {
        let key = account::text(params.get("session"), 256).ok_or_else(account::expired)?;
        let (_, pending) = self
            .pending
            .lock()
            .await
            .remove(key)
            .ok_or_else(account::expired)?;
        let client = pending.client.clone();
        let account = pending.complete().await?;
        Ok(json!({"ok":true,"credentials":self.finish_sign_in(client.as_ref(),account).await?}))
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let client = self.params_client(&params)?;
        let mut account = self.live(params.get("credentials")).await?;
        self.repick(client.as_ref(), &mut account).await?;
        self.token(client.as_ref(), &account, true).await?;
        Ok(json!({"changed":false}))
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        if context.credentials.is_none() {
            return Ok(
                json!({"models":[],"source":"preset","allowUnknownModels":false,"defaultAPI":"anthropic-messages","defaultBaseURL":network::BASE}),
            );
        }
        let client = self.client(context.network_proxy.as_ref())?;
        let mut account = self.live(context.credentials.as_ref()).await?;
        match self.listing(client.as_ref(), &mut account, true).await {
            Ok(rows) => Ok(models::output(&rows)),
            Err(error) if error.code != "authentication_required" => {
                let cache = self.models.lock().await;
                let Some((_, rows)) = cache.get(&account.token_key()) else {
                    return Err(error);
                };
                let mut result = models::output(rows);
                result["failedModelParts"] = json!(["zed|https://cloud.zed.dev/models"]);
                Ok(result)
            }
            Err(error) => Err(error),
        }
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let client = self.client(context.network_proxy.as_ref())?;
        let mut account = self.live(context.credentials.as_ref()).await?;
        let me = self.me(client.as_ref(), &account).await?;
        self.pick(&mut account, &me).await?;
        let name = account::plan_name(&account.plan);
        let mut items = vec![
            json!({"id":"plan","label":"Plan","value":name,"type":"info","countsForRouting":false}),
        ];
        if let Some(until) = me
            .pointer("/plan/subscription_period/ended_at")
            .and_then(Value::as_str)
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        {
            items.push(json!({"id":"period","label":"Subscription period ends","value":until.to_rfc3339(),"type":"info","countsForRouting":false}));
        }
        if me.pointer("/plan/has_overdue_invoices") == Some(&json!(true)) {
            items.push(json!({"id":"billing","label":"Billing","value":"Overdue invoice; hosted models are paused.","type":"info","countsForRouting":false}));
        }
        let mut value = nora_provider::usage::parsing::result(
            &json!({"account":{"id":context.account,"name":account.label,"plan":name}}),
            items,
            Vec::new(),
        );
        value["message"] =
            json!("Zed publishes the plan and period, but no remaining usage totals.");
        Ok(value)
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        let parsed = request::parse(&request)?;
        let wire = parsed.wire;
        let client = self.client(request.network_proxy.as_ref())?;
        let mut account = self.live(request.credentials.as_ref()).await?;
        let rows = match self.listing(client.as_ref(), &mut account, false).await {
            Ok(rows) => rows,
            Err(error) => return Ok(stream::failure(wire, status(&error))),
        };
        let Some(model) = rows.iter().find(|m| m.id == parsed.model) else {
            return Ok(stream::failure(wire, 404));
        };
        if models::wire(&model.vendor) != Some(wire) {
            return Ok(stream::failure(wire, 400));
        }
        let body = serde_json::to_vec(&parsed.envelope(&model.vendor))
            .map_err(|_| PluginError::invalid("The Zed envelope is invalid."))?;
        let mut renewed = false;
        let mut moved = false;
        loop {
            let token = match self.token(client.as_ref(), &account, renewed).await {
                Ok(token) => token,
                Err(error) if error.code == "zed_http_402" && !moved => {
                    moved = true;
                    if self.repick(client.as_ref(), &mut account).await? {
                        renewed = false;
                        continue;
                    }
                    return Ok(stream::failure(wire, 402));
                }
                Err(error) => return Ok(stream::failure(wire, status(&error))),
            };
            let outgoing = network::request(HttpMethod::Post, "/completions")
                .with_header("authorization", format!("Bearer {token}"))
                .with_header("x-zed-version", network::VERSION)
                .with_header("x-zed-client-supports-status-messages", "true")
                .with_header(
                    "x-zed-client-supports-stream-ended-request-completion-status",
                    "true",
                )
                .with_body(body.clone());
            let response = network::send(client.as_ref(), outgoing).await?;
            if !renewed && network::stale(&response) {
                renewed = true;
                continue;
            }
            if response.status == 402 && !moved {
                moved = true;
                drop(response);
                if self.repick(client.as_ref(), &mut account).await? {
                    renewed = false;
                    continue;
                }
                return Ok(stream::failure(wire, 402));
            }
            if !(200..300).contains(&response.status) {
                return Ok(stream::failure(wire, response.status));
            }
            return match stream::response(wire, parsed.streaming, response).await {
                Ok(response) => Ok(response),
                Err(_) => Ok(stream::failure(wire, 502)),
            };
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
