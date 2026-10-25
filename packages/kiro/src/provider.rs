use super::{
    auth, network, request,
    session::{self, Credential},
    source_kind::SourceKind,
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
#[path = "discovery.rs"]
mod discovery;
#[path = "owner.rs"]
mod owner;
#[path = "source_runtime.rs"]
mod source_runtime;

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
    pending: BTreeMap<String, OwnedRenewal>,
    key_profiles: BTreeMap<String, String>,
}
struct OwnedRenewal {
    next: Credential,
    needs_identity: bool,
}
pub(crate) struct Kiro {
    config: RwLock<Option<Config>>,
    client: Option<Arc<dyn HttpClient>>,
    persistence: Arc<dyn Persistence>,
    pending: Mutex<BTreeMap<String, auth::Pending>>,
    rotations: Mutex<Rotations>,
    owner: Arc<dyn owner::CliOwner>,
    sources: Mutex<BTreeMap<String, Arc<Mutex<source_runtime::SourceState>>>>,
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
            owner: Arc::new(owner::InstalledCli),
            sources: Mutex::new(BTreeMap::new()),
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
        Ok(Arc::new(
            ReqwestHttpClient::with_client(nora_provider_plugin_sdk::http_client(proxy)?)
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
                .map_err(|_| PluginError::invalid("The Kiro network proxy is invalid."))?;
        self.client(proxy.as_ref())
    }
    fn rotation_key(credential: &Credential) -> String {
        session::fingerprint(&format!(
            "owned\0{}\0{}\0{}\0{}\0{}",
            credential.access,
            credential.method,
            credential.profile,
            credential.realm,
            credential.account_identity().unwrap_or(Value::Null)
        ))
    }
    #[cfg(test)]
    async fn live(
        &self,
        value: Option<&Value>,
        client: &dyn HttpClient,
        force: bool,
    ) -> PluginResult<Credential> {
        self.live_with_proxy(value, client, force, None, None).await
    }
    async fn live_with_proxy(
        &self,
        value: Option<&Value>,
        client: &dyn HttpClient,
        force: bool,
        rejected: Option<&str>,
        proxy: Option<&NetworkProxy>,
    ) -> PluginResult<Credential> {
        let value = value.ok_or_else(session::expired)?;
        if let Some(source) = value.get("source") {
            let selected = SourceKind::from_reference(source)?;
            let path = session::text(source.get("path"), 16 * 1024).ok_or_else(session::expired)?;
            return self
                .source_live(&selected, path, client, Some(value), force, rejected, proxy)
                .await?
                .ok_or_else(session::expired);
        }
        let mut credential = Credential::from_saved(value)?;
        let mut state = self.rotations.lock().await;
        // A rotated refresh token is never used until the host confirms durable storage.
        for _ in 0..32 {
            let next_key = Self::rotation_key(&credential);
            if let Some(pending) = state.pending.get_mut(&next_key) {
                let next = self.finish_owned_renewal(pending, client, value).await?;
                state.pending.remove(&next_key);
                state.after.insert(next_key, next.clone());
                return Ok(next);
            }
            let Some(next) = state.after.get(&next_key) else {
                break;
            };
            if !next.matches_saved_identity(value) {
                return Err(session::expired());
            }
            credential = next.clone();
        }
        if credential.method != "apikey"
            && ((force && rejected.is_none_or(|access| access == credential.access))
                || !credential.fresh())
        {
            let old_key = Self::rotation_key(&credential);
            let next = network::refresh(client, &credential).await?;
            let expected = credential.saved();
            let needs_identity = next.profile != credential.profile
                || next.realm != credential.realm
                || next.identity.is_empty();
            let pending = state
                .pending
                .entry(old_key.clone())
                .or_insert(OwnedRenewal {
                    next,
                    needs_identity,
                });
            let next = self
                .finish_owned_renewal(pending, client, &expected)
                .await?;
            if state.after.len() >= 32 {
                state.after.clear();
            }
            state.pending.remove(&old_key);
            state.after.insert(old_key, next.clone());
            credential = next;
        }
        if credential.profile.is_empty() {
            if credential.method == "apikey" {
                let key = network::key_identity(&credential.access);
                credential.profile = if let Some(profile) = state.key_profiles.get(&key) {
                    profile.clone()
                } else {
                    let profile = network::profile(client, &credential).await?;
                    if state.key_profiles.len() >= 32 {
                        state.key_profiles.clear();
                    }
                    state.key_profiles.insert(key, profile.clone());
                    profile
                };
            } else {
                credential.profile = network::profile(client, &credential).await?;
            }
        }
        credential.runtime_region()?;
        Ok(credential)
    }
    async fn finish_owned_renewal(
        &self,
        pending: &mut OwnedRenewal,
        client: &dyn HttpClient,
        expected: &Value,
    ) -> PluginResult<Credential> {
        if pending.needs_identity {
            network::identify(client, &mut pending.next).await?;
            pending.needs_identity = false;
        }
        if !pending.next.matches_saved_identity(expected) {
            return Err(session::expired());
        }
        self.persistence.save(pending.next.saved()).await?;
        Ok(pending.next.clone())
    }
    fn source_saved(credential: &Credential, kind: &SourceKind, path: &str) -> Value {
        let mut saved = json!({"type":"oauth","authMethod":kind.auth_method(),"source":kind.reference(path),"accountId":credential.identity,"email":credential.email,"plan":credential.plan,"label":if credential.email.is_empty(){"Kiro account"}else{&credential.email},"profileArn":credential.profile,"region":credential.region,"userId":credential.user_id,"realm":credential.realm,"extras":{"noraAccountSource":kind.marker(path)}});
        // A new Kiro login has a new refresh token. Access tokens rotate during
        // ordinary use and must never make a deliberately removed account return.
        if !credential.refresh.is_empty() {
            saved["extras"]["noraAccountSource"]["loginMark"] = json!(session::fingerprint(
                &format!("{}\0{path}\0{}", kind.auth_method(), credential.refresh)
            ));
        }
        if let Some(identity) = credential.account_identity() {
            saved["accountIdentity"] = identity.clone();
            saved["extras"]["accountIdentity"] = identity;
        }
        saved
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
        Ok(
            json!({"apiVersion":API_VERSION,"capabilities":{"persistent":true,"accountDiscovery":true}}),
        )
    }
    async fn auth_methods(&self, _context: CallContext) -> PluginResult<Value> {
        Ok(self.config()?.methods)
    }
    async fn discover_accounts(&self, context: CallContext) -> PluginResult<Value> {
        discovery::discover(self, context).await
    }
    async fn accounts(&self, context: CallContext) -> PluginResult<Value> {
        let Some(value) = context.credentials.as_ref() else {
            return Ok(json!([]));
        };
        let saved = if let Some(source) = value.get("source") {
            let kind = SourceKind::from_reference(source)?;
            let path = session::text(source.get("path"), 16 * 1024).ok_or_else(session::expired)?;
            let client = self.client(context.network_proxy.as_ref())?;
            let credential = self
                .live_with_proxy(
                    Some(value),
                    client.as_ref(),
                    false,
                    None,
                    context.network_proxy.as_ref(),
                )
                .await?;
            Self::source_saved(&credential, &kind, path)
        } else {
            let mut credential = Credential::from_saved(value)?;
            if credential.method == "apikey" {
                credential.identity = network::key_identity(&credential.access);
                credential.saved()
            } else {
                if credential.user_id.is_empty()
                    && value
                        .get("accountIdentity")
                        .or_else(|| value.pointer("/extras/accountIdentity"))
                        .is_none()
                    && credential.fresh()
                {
                    let client = self.client(context.network_proxy.as_ref())?;
                    let mut verified = credential.clone();
                    // Legacy Kiro records kept only the verified email. A current
                    // session may now also reveal a stable user id; a temporary
                    // failure keeps the already saved profile available.
                    if network::identify(client.as_ref(), &mut verified)
                        .await
                        .is_ok()
                    {
                        credential = verified;
                    }
                }
                if credential.account_identity().is_none() {
                    let client = self.client(context.network_proxy.as_ref())?;
                    if credential.fresh() {
                        network::identify(client.as_ref(), &mut credential).await?;
                    } else {
                        credential = self
                            .live_with_proxy(
                                Some(value),
                                client.as_ref(),
                                false,
                                None,
                                context.network_proxy.as_ref(),
                            )
                            .await?;
                        if credential.account_identity().is_none() {
                            network::identify(client.as_ref(), &mut credential).await?;
                        }
                    }
                }
                if !credential.matches_saved_identity(value) {
                    return Err(session::expired());
                }
                credential.saved()
            }
        };
        let mut profile = json!({"id":context.account});
        for key in ["email", "plan", "accountId"] {
            if let Some(text) = session::text(saved.get(key), 512) {
                profile[key] = json!(text);
            }
        }
        let mut result = json!({"accounts":[profile]});
        if saved != *value {
            result["credentials"] = saved;
        }
        Ok(result)
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
                credential.identity = network::key_identity(&credential.access);
                let mut verified = credential.clone();
                if network::identify(client.as_ref(), &mut verified)
                    .await
                    .is_ok()
                {
                    credential = verified;
                } else {
                    // A key remains usable without optional account metadata.
                    // Its profile only determines the region used by later calls.
                    credential.profile = verified.profile;
                }
                let mut state = self.rotations.lock().await;
                if state.key_profiles.len() >= 32 {
                    state.key_profiles.clear();
                }
                state.key_profiles.insert(
                    network::key_identity(&credential.access),
                    credential.profile.clone(),
                );
                Ok(json!({"ok":true,"credentials":credential.saved()}))
            }
            "cliSource" | "ideSource" | "magpieSource" => {
                let path = session::text(inputs.get("sourcePath"), 16 * 1024)
                    .ok_or_else(|| PluginError::invalid("Select one Kiro account file."))?
                    .to_owned();
                let kind = SourceKind::from_auth_method(method, inputs)?;
                let proxy: Option<NetworkProxy> = serde_json::from_value(
                    params.get("networkProxy").cloned().unwrap_or(Value::Null),
                )
                .map_err(|_| PluginError::invalid("The Kiro network proxy is invalid."))?;
                let credential = self
                    .source_live(
                        &kind,
                        &path,
                        client.as_ref(),
                        None,
                        false,
                        None,
                        proxy.as_ref(),
                    )
                    .await?
                    .ok_or_else(session::expired)?;
                Ok(json!({"ok":true,"credentials":Self::source_saved(&credential,&kind,&path)}))
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
        let client = self.params_client(&params)?;
        let proxy: Option<NetworkProxy> =
            serde_json::from_value(params.get("networkProxy").cloned().unwrap_or(Value::Null))
                .map_err(|_| PluginError::invalid("The Kiro network proxy is invalid."))?;
        self.live_with_proxy(
            Some(value),
            client.as_ref(),
            params["force"] == true,
            None,
            proxy.as_ref(),
        )
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
            .live_with_proxy(
                context.credentials.as_ref(),
                client.as_ref(),
                false,
                None,
                context.network_proxy.as_ref(),
            )
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
            .live_with_proxy(
                context.credentials.as_ref(),
                client.as_ref(),
                false,
                None,
                context.network_proxy.as_ref(),
            )
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
            .live_with_proxy(
                credentials.as_ref(),
                client.as_ref(),
                false,
                None,
                proxy.as_ref(),
            )
            .await?;
        let budget = request::thinking(&body, &model, effort.as_deref());
        let profile = if credential.method == "apikey" {
            ""
        } else {
            &credential.profile
        };
        let (payload, names) = request::build(&body, &model, profile, budget)?;
        let mut response = self.send(client.as_ref(), &credential, &payload).await?;
        if response.status == 403 && credential.method != "apikey" {
            let refreshed = self
                .live_with_proxy(
                    credentials.as_ref(),
                    client.as_ref(),
                    true,
                    Some(&credential.access),
                    proxy.as_ref(),
                )
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
