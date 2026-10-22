use super::{
    auth, network, request,
    session::{self, Credential},
    stream, usage,
};
use async_trait::async_trait;
use nora_ai::{HttpClient, ReqwestHttpClient};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, PluginError,
    PluginResult, ProviderPlugin, API_VERSION,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
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
    installation: String,
}
#[derive(Default)]
struct Rotations {
    after: BTreeMap<String, Credential>,
    pending: BTreeMap<String, Credential>,
    verified: BTreeMap<String, Credential>,
}
pub(crate) struct Kiro {
    config: RwLock<Option<Config>>,
    client: Option<Arc<dyn HttpClient>>,
    persistence: Arc<dyn Persistence>,
    pending: Mutex<BTreeMap<String, auth::Pending>>,
    rotations: Mutex<Rotations>,
    windows: Mutex<BTreeMap<String, u64>>,
}
impl Default for Kiro {
    fn default() -> Self {
        Self {
            config: RwLock::new(None),
            client: None,
            persistence: Arc::new(Host),
            pending: Mutex::new(BTreeMap::new()),
            rotations: Mutex::new(Rotations::default()),
            windows: Mutex::new(BTreeMap::new()),
        }
    }
}
impl Kiro {
    fn config(&self) -> PluginResult<Config> {
        self.config
            .read()
            .map_err(|_| PluginError::invalid("The Kiro configuration is unavailable."))?
            .clone()
            .ok_or_else(|| PluginError::invalid("Initialize Kiro before using it."))
    }
    fn client(&self, proxy: Option<&NetworkProxy>) -> PluginResult<Arc<dyn HttpClient>> {
        let config = self.config()?;
        let proxy = proxy.or(config.proxy.as_ref());
        if let Some(proxy) = proxy {
            proxy.validate()?;
        }
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        Ok(Arc::new(ReqwestHttpClient::with_client(
            nora_provider_plugin_sdk::http_client(proxy)?,
        )))
    }
    fn params_client(&self, params: &Value) -> PluginResult<Arc<dyn HttpClient>> {
        let proxy: Option<NetworkProxy> =
            serde_json::from_value(params.get("networkProxy").cloned().unwrap_or(Value::Null))
                .map_err(|_| PluginError::invalid("The Kiro network proxy is invalid."))?;
        self.client(proxy.as_ref())
    }
    async fn live(
        &self,
        value: Option<&Value>,
        client: &dyn HttpClient,
        force: bool,
    ) -> PluginResult<Credential> {
        let value = value.ok_or_else(session::expired)?;
        if let Some(source) = value.get("source") {
            let kind = session::text(source.get("kind"), 16)
                .ok_or_else(session::expired)?
                .to_owned();
            let path = session::text(source.get("path"), 16 * 1024)
                .ok_or_else(session::expired)?
                .to_owned();
            let expected = session::text(value.get("accountId"), 128)
                .ok_or_else(session::expired)?
                .to_owned();
            let mut credential =
                tokio::task::spawn_blocking(move || session::read_source(&kind, &path))
                    .await
                    .map_err(|_| session::expired())??;
            if credential.expires > 0 && credential.expires <= session::now() {
                return Err(session::expired());
            }
            let key = session::fingerprint(&format!("source\0{}\0{expected}", credential.access));
            let mut state = self.rotations.lock().await;
            if let Some(cached) = state.verified.get(&key).filter(|_| !force) {
                return Ok(cached.clone());
            }
            network::identify(client, &mut credential).await?;
            if credential.email.is_empty() || credential.identity != expected {
                return Err(session::expired());
            }
            if state.verified.len() >= 32 {
                state.verified.clear();
            }
            state.verified.insert(key, credential.clone());
            return Ok(credential);
        }
        let mut credential = Credential::from_saved(value)?;
        let mut state = self.rotations.lock().await;
        // A rotated refresh token is never used until the host confirms durable storage.
        for _ in 0..32 {
            let next_key = session::fingerprint(&credential.access);
            if let Some(pending) = state.pending.get(&next_key).cloned() {
                self.persistence.save(pending.saved()).await?;
                state.pending.remove(&next_key);
                state.after.insert(next_key, pending.clone());
                return Ok(pending);
            }
            let Some(next) = state.after.get(&next_key) else {
                break;
            };
            credential = next.clone();
        }
        if credential.method != "apikey" && (force || !credential.fresh()) {
            let old_key = session::fingerprint(&credential.access);
            let mut next = network::refresh(client, &credential).await?;
            let expected = credential.identity.clone();
            if next.profile != credential.profile || next.identity.is_empty() {
                network::identify(client, &mut next).await?;
            }
            if !expected.is_empty() && !next.identity.is_empty() && next.identity != expected {
                return Err(session::expired());
            }
            if state.after.len() >= 32 {
                state.after.clear();
            }
            state.pending.insert(old_key.clone(), next.clone());
            self.persistence.save(next.saved()).await?;
            state.pending.remove(&old_key);
            state.after.insert(old_key, next.clone());
            credential = next;
        }
        if credential.profile.is_empty() {
            credential.profile = network::profile(client, &credential).await?;
        }
        credential.runtime_region()?;
        Ok(credential)
    }
    fn source_saved(credential: &Credential, kind: &str, path: &str) -> Value {
        json!({"type":"oauth","authMethod":if kind=="cli"{"cliSource"}else{"ideSource"},"source":{"kind":kind,"path":path},"accountId":credential.identity,"email":credential.email,"plan":credential.plan,"label":if credential.email.is_empty(){"Kiro account"}else{&credential.email}})
    }
    async fn send(
        &self,
        client: &dyn HttpClient,
        credential: &Credential,
        body: &Value,
    ) -> PluginResult<nora_ai::HttpStreamingResponse> {
        let region = credential.runtime_region()?;
        let ua=format!("aws-sdk-rust/1.0.0 ua/2.1 os/other lang/rust api/codewhispererstreaming#1.28.3 m/E app/AmazonQ-For-CLI md/appVersion-1.28.3-{}",nora_ai::oauth_pkce::random_hex(16));
        let outgoing = network::authorize(
            network::post(
                format!("https://runtime.{region}.kiro.dev/generateAssistantResponse"),
                body,
            )?,
            credential,
        )
        .with_header("accept", "application/vnd.amazon.eventstream")
        .with_header("x-amzn-codewhisperer-optout", "true")
        .with_header("amz-sdk-invocation-id", request::uuid())
        .with_header("amz-sdk-request", "attempt=1; max=1")
        .with_header("x-amzn-kiro-agent-mode", "vibe")
        .with_header("x-amz-user-agent", &ua)
        .with_header("user-agent", &ua);
        tokio::time::timeout(std::time::Duration::from_secs(60), client.stream(outgoing))
            .await
            .map_err(|_| PluginError::transport("The Kiro request timed out."))?
            .map_err(|_| PluginError::transport("The Kiro service could not be reached."))
    }
}
#[async_trait]
impl ProviderPlugin for Kiro {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        if params.api_version != API_VERSION
            || params.plugin_id != "kiro"
            || params.manifest["id"] != "kiro"
            || params.manifest["baseURL"] != session::BASE
        {
            return Err(PluginError::invalid(
                "The Kiro executable does not match its manifest.",
            ));
        }
        let proxy: Option<NetworkProxy> = serde_json::from_value(
            params
                .manifest
                .pointer("/config/networkProxy")
                .cloned()
                .unwrap_or(Value::Null),
        )
        .map_err(|_| PluginError::invalid("The Kiro network proxy is invalid."))?;
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        *self.config.write().map_err(|_| session::expired())? = Some(Config {
            proxy,
            methods: params.manifest["authMethods"].clone(),
            installation: params.data_directory.to_string_lossy().into(),
        });
        Ok(json!({"apiVersion":API_VERSION,"capabilities":{"persistent":true}}))
    }
    async fn auth_methods(&self, _context: CallContext) -> PluginResult<Value> {
        Ok(self.config()?.methods)
    }
    async fn accounts(&self, context: CallContext) -> PluginResult<Value> {
        let Some(value) = context.credentials else {
            return Ok(json!([]));
        };
        let mut profile = json!({"id":context.account});
        for key in ["email", "plan", "accountId"] {
            if let Some(value) = session::text(value.get(key), 512) {
                profile[key] = json!(value);
            }
        }
        Ok(json!([profile]))
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        let client = self.params_client(&params)?;
        let inputs = params
            .get("inputs")
            .or_else(|| params.get("fields"))
            .unwrap_or(&params);
        let method = params
            .get("authMethodID")
            .or_else(|| params.get("method"))
            .and_then(Value::as_str)
            .unwrap_or("oauth");
        match method {
            "oauth" => {
                let (pending, url) = auth::start(client, &self.config()?.installation).await?;
                let session = nora_ai::oauth_pkce::random_hex(24);
                let mut sessions = self.pending.lock().await;
                if sessions.len() >= 8 {
                    sessions.clear();
                }
                sessions.insert(session.clone(), pending);
                Ok(
                    json!({"session":session,"url":url,"method":"auto","instructions":"Sign in to Kiro with Google, GitHub, AWS Builder ID or IAM Identity Center."}),
                )
            }
            "apiKey" => {
                let mut credential = Credential::api_key(inputs)?;
                network::identify(client.as_ref(), &mut credential).await?;
                Ok(json!({"ok":true,"credentials":credential.saved()}))
            }
            "cliSource" | "ideSource" => {
                let path = session::text(inputs.get("sourcePath"), 16 * 1024)
                    .ok_or_else(|| PluginError::invalid("Select one Kiro account file."))?
                    .to_owned();
                let kind = if method == "cliSource" { "cli" } else { "ide" };
                let selected = path.clone();
                let mut credential =
                    tokio::task::spawn_blocking(move || session::read_source(kind, &selected))
                        .await
                        .map_err(|_| session::expired())??;
                if credential.expires > 0 && credential.expires <= session::now() {
                    return Err(session::expired());
                }
                network::identify(client.as_ref(), &mut credential).await?;
                if credential.email.is_empty() {
                    return Err(PluginError::authentication(
                        "Kiro did not identify the selected source's account.",
                    ));
                }
                Ok(json!({"ok":true,"credentials":Self::source_saved(&credential,kind,&path)}))
            }
            _ => Err(PluginError::unsupported(method)),
        }
    }
    async fn callback(&self, params: Value) -> PluginResult<Value> {
        let key = session::text(params.get("session"), 256).ok_or_else(session::expired)?;
        let pending = self
            .pending
            .lock()
            .await
            .remove(key)
            .ok_or_else(session::expired)?;
        let credential = pending.complete().await?;
        Ok(json!({"ok":true,"credentials":credential.saved()}))
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let value = params.get("credentials").ok_or_else(session::expired)?;
        if value.get("source").is_some() {
            return Ok(json!({"changed":false}));
        }
        let client = self.params_client(&params)?;
        self.live(Some(value), client.as_ref(), params["force"] == true)
            .await?;
        Ok(json!({"changed":false}))
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        if context.credentials.is_none() {
            return Ok(
                json!({"models":[],"source":"preset","allowUnknownModels":true,"defaultAPI":"anthropic-messages","defaultBaseURL":session::BASE}),
            );
        }
        let client = self.client(context.network_proxy.as_ref())?;
        let credential = self
            .live(context.credentials.as_ref(), client.as_ref(), false)
            .await?;
        let output = usage::models(
            &network::management(client.as_ref(), &credential, "List-Available-Models", None)
                .await?,
        )?;
        let mut windows = self.windows.lock().await;
        for model in output["models"].as_array().into_iter().flatten() {
            if let (Some(id), Some(window)) =
                (model["id"].as_str(), model["contextWindow"].as_u64())
            {
                windows.insert(id.into(), window);
            }
        }
        Ok(output)
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let client = self.client(context.network_proxy.as_ref())?;
        let credential = self
            .live(context.credentials.as_ref(), client.as_ref(), false)
            .await?;
        let mut value = usage::parse(
            &network::management(client.as_ref(), &credential, "Get-Usage-Limits", None).await?,
        )?;
        value["account"]["id"] = json!(context.account);
        Ok(value)
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        let url = url::Url::parse(&request.url)
            .map_err(|_| PluginError::invalid("The Kiro request address is invalid."))?;
        if request.method != "POST"
            || url.origin() != url::Url::parse(session::BASE).unwrap().origin()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || !url
                .query_pairs()
                .all(|(key, value)| key == "beta" && value == "true")
            || !["/v1/messages", "/messages"].contains(&url.path())
        {
            return Err(PluginError::invalid(
                "Kiro accepts Anthropic Messages requests at its official endpoint.",
            ));
        }
        let credentials = request.credentials.clone();
        let proxy = request.network_proxy.clone();
        let effort = request.metadata.get("reasoningEffort").cloned();
        let body = tokio::task::spawn_blocking(move || {
            let body = request
                .decode_body()?
                .ok_or_else(|| PluginError::invalid("The Kiro request body is missing."))?;
            serde_json::from_slice::<Value>(&body)
                .map_err(|_| PluginError::invalid("The Kiro request body is invalid JSON."))
        })
        .await
        .map_err(|_| PluginError::invalid("The Kiro request body could not be read."))??;
        let model = session::text(body.get("model"), 256)
            .filter(|id| {
                id.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-:".contains(&b))
            })
            .ok_or_else(|| PluginError::invalid("The Kiro model ID is invalid."))?
            .to_owned();
        let client = self.client(proxy.as_ref())?;
        let credential = self
            .live(credentials.as_ref(), client.as_ref(), false)
            .await?;
        let budget = request::thinking(&body, &model, effort.as_deref());
        let (payload, names) = request::build(&body, &model, &credential.profile, budget)?;
        let mut response = self.send(client.as_ref(), &credential, &payload).await?;
        if response.status == 403 && credential.method != "apikey" {
            let refreshed = self
                .live(credentials.as_ref(), client.as_ref(), true)
                .await?;
            if refreshed.access != credential.access {
                let (payload, _) = request::build(&body, &model, &refreshed.profile, budget)?;
                response = self.send(client.as_ref(), &refreshed, &payload).await?;
            }
        }
        let context = self.windows.lock().await.get(&model).copied().unwrap_or(0);
        stream::response(
            response,
            model,
            body["stream"] == true,
            names,
            budget,
            context,
        )
        .await
    }
}
#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
