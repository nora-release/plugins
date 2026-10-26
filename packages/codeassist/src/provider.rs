use super::*;
use async_trait::async_trait;
use nora_ai::{HttpClient, HttpMethod, HttpRequest, ReqwestHttpClient};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, ProviderPlugin,
    API_VERSION,
};
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use tokio::sync::Mutex;

#[derive(Clone)]
struct Config {
    proxy: Option<NetworkProxy>,
    methods: Value,
}
pub struct CodeAssist {
    pub(crate) app: App,
    config: RwLock<Option<Config>>,
    pending: Mutex<BTreeMap<String, auth::Pending>>,
    tokens: Mutex<BTreeMap<String, session::Tokens>>,
    projects: Mutex<BTreeMap<String, project::Project>>,
    client: Option<Arc<dyn HttpClient>>,
    version: Mutex<Option<(std::time::Instant, String)>>,
    experiments: Mutex<BTreeMap<String, (std::time::Instant, models::Flags)>>,
    catalogs: Mutex<BTreeMap<String, (std::time::Instant, Value)>>,
    persistence: Arc<dyn nora_provider::account_import::CredentialPersistence>,
}
impl CodeAssist {
    pub fn new(app: App) -> Self {
        Self {
            app,
            config: RwLock::new(None),
            pending: Mutex::new(BTreeMap::new()),
            tokens: Mutex::new(BTreeMap::new()),
            projects: Mutex::new(BTreeMap::new()),
            client: None,
            version: Mutex::new(None),
            experiments: Mutex::new(BTreeMap::new()),
            catalogs: Mutex::new(BTreeMap::new()),
            persistence: Arc::new(nora_provider::account_import::HostPersistence),
        }
    }
    fn config(&self) -> PluginResult<Config> {
        self.config
            .read()
            .map_err(|_| invalid("The Code Assist configuration is unavailable."))?
            .clone()
            .ok_or_else(|| invalid("Initialize Code Assist before using it."))
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
                .map_err(|_| invalid("The Code Assist network proxy is invalid."))?;
        self.client(proxy.as_ref())
    }
    pub(crate) async fn client_version(&self, client: &dyn HttpClient) -> String {
        if self.app != App::Antigravity {
            return String::new();
        }
        let mut cached = self.version.lock().await;
        if let Some((_, version)) = cached
            .as_ref()
            .filter(|(at, _)| at.elapsed() < std::time::Duration::from_secs(21600))
        {
            return version.clone();
        }
        let version = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let response = client
                .stream(
                    HttpRequest::new(HttpMethod::Get, VERSION)
                        .with_header("user-agent", "electron-builder"),
                )
                .await
                .ok()?;
            if response.status != 200 {
                return None;
            }
            let text = nora_ai::collect_http_body_text_limited(response.body, 64 * 1024 + 1)
                .await
                .ok()?;
            if text.len() > 64 * 1024 {
                return None;
            }
            text.lines()
                .find_map(|line| line.strip_prefix("version:"))
                .map(|value| value.trim().trim_matches(['\'', '"']))
                .filter(|version| {
                    !version.is_empty()
                        && version.len() <= 64
                        && version
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || byte == b'.')
                })
                .map(str::to_owned)
        })
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| {
            cached
                .as_ref()
                .map(|(_, version)| version.clone())
                .unwrap_or_else(|| "2.9.1".into())
        });
        *cached = Some((std::time::Instant::now(), version.clone()));
        version
    }
    async fn live(
        &self,
        value: Option<&Value>,
        client: &dyn HttpClient,
        force: bool,
    ) -> PluginResult<session::Tokens> {
        if let Some(value) = value.filter(|value| gemini_source::bound(value)) {
            if self.app != App::GeminiCLI
                || value.pointer("/source/app").and_then(Value::as_str) != Some(self.app.id())
            {
                return Err(expired());
            }
            let path = text(value.pointer("/source/path"), 16 * 1024).ok_or_else(expired)?;
            let uid = text(value.pointer("/source/uid"), 128).ok_or_else(expired)?;
            let snapshot = gemini_source::Snapshot::read(std::path::Path::new(path))?;
            return self
                .live_gemini_source(snapshot, Some(uid), client, force)
                .await;
        }
        let (mut tokens, expected) = session::current(value, self.app)?;
        let version = self.client_version(client).await;
        let key = fingerprint(&format!(
            "{}\0{}\0{}",
            tokens.refresh,
            tokens.project,
            expected.as_deref().unwrap_or_default()
        ));
        let mut cache = self.tokens.lock().await;
        if !force {
            if let Some(cached) = cache
                .get(&key)
                .filter(|cached| cached.expires > now() + 120_000)
            {
                let mut cached = cached.clone();
                cached.version = version;
                return Ok(cached);
            }
        }
        if force || tokens.access.is_empty() || tokens.expires <= now() + 120_000 {
            let payload = auth::exchange(
                self.app,
                &[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", &tokens.refresh),
                ],
                client,
            )
            .await?;
            tokens = auth::read_tokens(&payload, Some(&tokens))?;
        }
        if expected.is_some() || tokens.uid.is_empty() {
            auth::identify(&mut tokens, client).await?;
        }
        if expected.as_ref().is_some_and(|uid| *uid != tokens.uid) {
            return Err(expired());
        }
        if cache.len() >= 32 {
            cache.retain(|_, tokens| tokens.expires > now());
            if cache.len() >= 32 {
                cache.clear();
            }
        }
        tokens.version = version;
        cache.insert(key, tokens.clone());
        Ok(tokens)
    }
    async fn live_gemini_source(
        &self,
        snapshot: gemini_source::Snapshot,
        expected: Option<&str>,
        client: &dyn HttpClient,
        force: bool,
    ) -> PluginResult<session::Tokens> {
        nora_provider_plugin_sdk::require_subscription_accounts()?;
        let mut cache = self.tokens.lock().await;
        if !force {
            if let Some(cached) = cache
                .get(&snapshot.key)
                .filter(|tokens| tokens.expires > now() + 120_000)
            {
                if expected.is_some_and(|uid| uid != cached.uid) {
                    return Err(expired());
                }
                snapshot.ensure_current()?;
                return Ok(cached.clone());
            }
        }
        let mut tokens = snapshot.tokens.clone();
        let renewed = force || tokens.access.is_empty() || tokens.expires <= now() + 120_000;
        if renewed {
            let reply = auth::exchange(
                App::GeminiCLI,
                &[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", &tokens.refresh),
                ],
                client,
            )
            .await?;
            tokens = auth::read_tokens(&reply, Some(&tokens))?;
        }
        auth::identify(&mut tokens, client).await?;
        if expected.is_some_and(|uid| uid != tokens.uid) {
            return Err(expired());
        }
        snapshot.ensure_current()?;
        if renewed {
            snapshot.write_renewed(&tokens)?;
        }
        if cache.len() >= 32 {
            cache.retain(|_, tokens| tokens.expires > now());
        }
        if cache.len() >= 32 {
            cache.clear();
        }
        cache.insert(snapshot.key, tokens.clone());
        Ok(tokens)
    }
    async fn discover_gemini_at(
        &self,
        path: &std::path::Path,
        client: &dyn HttpClient,
    ) -> PluginResult<Value> {
        if !path.exists() {
            return Ok(json!({"accounts":[]}));
        }
        let snapshot = gemini_source::Snapshot::read(path)?;
        let tokens = self
            .live_gemini_source(snapshot, None, client, false)
            .await?;
        Ok(
            json!({"accounts":[{"sourceID":gemini_source::METHOD,"credentials":gemini_source::saved(&tokens,path)?}]}),
        )
    }
    pub(crate) async fn project(
        &self,
        tokens: &session::Tokens,
        client: &dyn HttpClient,
    ) -> PluginResult<project::Project> {
        let key = fingerprint(&format!("{}\0{}", tokens.refresh, tokens.project));
        let mut projects = self.projects.lock().await;
        if let Some(project) = projects.get(&key) {
            return Ok(project.clone());
        }
        let project = project::load(self.app, tokens, client).await?;
        if projects.len() >= 32 {
            projects.clear();
        }
        projects.insert(key, project.clone());
        Ok(project)
    }
    async fn call(
        &self,
        tokens: &session::Tokens,
        method: &str,
        body: &Value,
        client: &dyn HttpClient,
    ) -> PluginResult<Value> {
        auth::json_request(
            client,
            auth::authorized(self.app, tokens, self.app.base(), method, body)?,
        )
        .await
    }
    async fn flags(
        &self,
        tokens: &session::Tokens,
        project: &str,
        client: &dyn HttpClient,
    ) -> models::Flags {
        let key = fingerprint(&format!("{}\0{project}", tokens.refresh));
        let mut cache = self.experiments.lock().await;
        if let Some((_, flags)) = cache
            .get(&key)
            .filter(|(at, _)| at.elapsed() < std::time::Duration::from_secs(3600))
        {
            return flags.clone();
        }
        let platform = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => "DARWIN_ARM64",
            ("macos", _) => "DARWIN_AMD64",
            ("linux", "aarch64") => "LINUX_ARM64",
            ("linux", _) => "LINUX_AMD64",
            ("windows", _) => "WINDOWS_AMD64",
            _ => "PLATFORM_UNSPECIFIED",
        };
        let flags = self.call(tokens,"listExperiments",&json!({"project":project,"metadata":{"ideName":"IDE_UNSPECIFIED","pluginType":"GEMINI","ideVersion":"0.61.0","platform":platform,"updateChannel":"stable","duetProject":project}}),client).await.map(|value|models::Flags::parse(&value)).unwrap_or_default();
        if cache.len() >= 32 {
            cache.clear();
        }
        cache.insert(key, (std::time::Instant::now(), flags.clone()));
        flags
    }
    async fn antigravity_models(
        &self,
        tokens: &session::Tokens,
        project: &project::Project,
        client: &dyn HttpClient,
        force: bool,
    ) -> PluginResult<Value> {
        let key = fingerprint(&format!("{}\0{}", tokens.uid, project.id));
        let mut cache = self.catalogs.lock().await;
        if !force {
            if let Some((_, payload)) = cache
                .get(&key)
                .filter(|(at, _)| at.elapsed() < std::time::Duration::from_secs(300))
            {
                return Ok(payload.clone());
            }
        }
        let mut error = PluginError::transport("Antigravity could not list this account's models.");
        for tier in project.entitlements() {
            let mut body = json!({"project":project.id});
            if !tier.is_empty() {
                body["entitlement"] = json!({"userTier":tier});
            }
            match self
                .call(tokens, "fetchAvailableModels", &body, client)
                .await
            {
                Ok(value) => {
                    models::antigravity(&value)?;
                    if cache.len() >= 32 {
                        cache.clear();
                    }
                    cache.insert(key, (std::time::Instant::now(), value.clone()));
                    return Ok(value);
                }
                Err(next) => error = next,
            }
        }
        Err(error)
    }
    async fn finish_login(
        &self,
        mut tokens: session::Tokens,
        path: Option<&str>,
        client: &dyn HttpClient,
    ) -> PluginResult<Value> {
        tokens.version = self.client_version(client).await;
        let project = self.project(&tokens, client).await.ok();
        if let Some(project) = &project {
            tokens.project = project.id.clone();
        }
        let mut credentials = session::saved(&tokens, self.app, path);
        if let Some(project) = project {
            credentials["plan"] = json!(project.plan);
        }
        Ok(json!({"ok":true,"credentials":credentials}))
    }
}
#[async_trait]
impl ProviderPlugin for CodeAssist {
    async fn call(&self, method: &str, params: Value) -> PluginResult<Value> {
        match method {
            "inspectAccountImport" => account_import::inspect(self.app, &params),
            "importAccount" => {
                account_import::import(
                    self,
                    &params,
                    self.params_client(&params)?.as_ref(),
                    &nora_provider::account_import::HostPersistence,
                )
                .await
            }
            _ => Err(PluginError::unsupported(method)),
        }
    }
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        if params.api_version != API_VERSION
            || params.plugin_id != self.app.id()
            || params.manifest["id"] != self.app.id()
            || params.manifest["baseURL"] != self.app.base()
        {
            return Err(invalid(
                "The Code Assist executable does not match its manifest.",
            ));
        }
        let proxy: Option<NetworkProxy> = serde_json::from_value(
            params
                .manifest
                .pointer("/config/networkProxy")
                .cloned()
                .unwrap_or(Value::Null),
        )
        .map_err(|_| invalid("The Code Assist network proxy is invalid."))?;
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        *self
            .config
            .write()
            .map_err(|_| invalid("The Code Assist configuration is unavailable."))? =
            Some(Config {
                proxy,
                methods: params.manifest["authMethods"].clone(),
            });
        Ok(
            json!({"apiVersion":API_VERSION,"capabilities":{"persistent":true,"accountDiscovery":self.app==App::GeminiCLI,"sdkFeatures":{"accountImport":1}}}),
        )
    }
    async fn auth_methods(&self, _context: CallContext) -> PluginResult<Value> {
        Ok(self.config()?.methods)
    }
    async fn accounts(&self, context: CallContext) -> PluginResult<Value> {
        let Some(mut value) = context.credentials else {
            return Ok(json!([]));
        };
        if value.get("accountIdentity").is_none() {
            let client = self.client(context.network_proxy.as_ref())?;
            let mut tokens = self.live(Some(&value), client.as_ref(), false).await?;
            if tokens.identity.is_none() {
                auth::identify(&mut tokens, client.as_ref()).await?;
            }
            if text(value.get("accountId"), 128).is_some_and(|uid| uid != tokens.uid) {
                return Err(expired());
            }
            let path = text(value.pointer("/source/path"), 16 * 1024);
            let mut credentials = if gemini_source::bound(&value) {
                gemini_source::saved(&tokens, std::path::Path::new(path.ok_or_else(expired)?))?
            } else {
                session::saved(&tokens, self.app, path)
            };
            nora_provider::account_import::preserve_metadata(&value, &mut credentials);
            if let Some(plan) = value.get("plan") {
                credentials["plan"] = plan.clone();
            }
            self.persistence.save(credentials.clone()).await?;
            value = credentials;
        }
        let mut profile = json!({"id":context.account});
        for key in ["email", "name", "plan", "accountId"] {
            if let Some(value) = text(value.get(key), 512) {
                profile[key] = json!(value);
            }
        }
        Ok(json!([profile]))
    }
    async fn discover_accounts(&self, context: CallContext) -> PluginResult<Value> {
        nora_provider_plugin_sdk::require_subscription_accounts()?;
        if self.app != App::GeminiCLI {
            return Ok(json!({"accounts":[]}));
        }
        let path = gemini_source::default_path()?;
        let client = self.client(context.network_proxy.as_ref())?;
        self.discover_gemini_at(&path, client.as_ref()).await
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        let client = self.params_client(&params)?;
        let inputs = params
            .get("inputs")
            .or_else(|| params.get("fields"))
            .unwrap_or(&params);
        let project = text(inputs.get("project"), 512)
            .unwrap_or_default()
            .to_owned();
        if project.chars().any(char::is_whitespace) {
            return Err(invalid("Enter a Google Cloud project ID without spaces."));
        }
        match params
            .get("authMethodID")
            .or_else(|| params.get("method"))
            .and_then(Value::as_str)
            .unwrap_or("oauth")
        {
            gemini_source::METHOD if self.app == App::GeminiCLI => {
                nora_provider_plugin_sdk::require_subscription_accounts()?;
                let path = gemini_source::default_path()?;
                let snapshot = gemini_source::Snapshot::read(&path)?;
                let tokens = self
                    .live_gemini_source(snapshot, None, client.as_ref(), false)
                    .await?;
                Ok(json!({"ok":true,"credentials":gemini_source::saved(&tokens,&path)?}))
            }
            "oauth" => {
                let (pending, url) = auth::start(self.app, project, client).await?;
                let session = nora_ai::oauth_pkce::random_hex(24);
                let mut sessions = self.pending.lock().await;
                if sessions.len() >= 8 {
                    sessions.clear();
                }
                sessions.insert(session.clone(), pending);
                Ok(json!({"session":session,"url":url,"method":"auto"}))
            }
            "source" => {
                let path = text(inputs.get("sourcePath"), 16 * 1024)
                    .ok_or_else(|| invalid("Select one Google account JSON file."))?;
                let mut tokens = session::selected(path, self.app)?;
                if !project.is_empty() {
                    tokens.project = project;
                }
                if tokens.access.is_empty() || tokens.expires <= now() + 120_000 {
                    let payload = auth::exchange(
                        self.app,
                        &[
                            ("grant_type", "refresh_token"),
                            ("refresh_token", &tokens.refresh),
                        ],
                        client.as_ref(),
                    )
                    .await?;
                    tokens = auth::read_tokens(&payload, Some(&tokens))?;
                }
                auth::identify(&mut tokens, client.as_ref()).await?;
                self.finish_login(tokens, Some(path), client.as_ref()).await
            }
            method => Err(PluginError::unsupported(method)),
        }
    }
    async fn callback(&self, params: Value) -> PluginResult<Value> {
        let key = text(params.get("session"), 256).ok_or_else(expired)?;
        let pending = self.pending.lock().await.remove(key).ok_or_else(expired)?;
        let client = pending.client.clone();
        let tokens = auth::complete(pending, self.app).await?;
        self.finish_login(tokens, None, client.as_ref()).await
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let client = self.params_client(&params)?;
        let value = params.get("credentials").ok_or_else(expired)?;
        let force = params["force"] == true;
        let tokens = self.live(Some(value), client.as_ref(), force).await?;
        if value.get("source").is_some() {
            return Ok(json!({"changed":false}));
        }
        let mut credentials = session::saved(&tokens, self.app, None);
        nora_provider::account_import::preserve_metadata(value, &mut credentials);
        if let Some(plan) = value.get("plan") {
            credentials["plan"] = plan.clone();
        }
        Ok(json!({"credentials":credentials}))
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        if context.credentials.is_none() {
            return Ok(
                json!({"models":[],"source":"preset","allowUnknownModels":false,"defaultAPI":"google-generative-ai","defaultBaseURL":self.app.base()}),
            );
        }
        let client = self.client(context.network_proxy.as_ref())?;
        let tokens = self
            .live(context.credentials.as_ref(), client.as_ref(), false)
            .await?;
        let project = self.project(&tokens, client.as_ref()).await?;
        if self.app == App::Antigravity {
            return Ok(models::encode(
                self.app,
                models::antigravity(
                    &self
                        .antigravity_models(&tokens, &project, client.as_ref(), true)
                        .await?,
                )?
                .models,
            ));
        }
        let quota = self
            .call(
                &tokens,
                "retrieveUserQuota",
                &json!({"project":project.id}),
                client.as_ref(),
            )
            .await?;
        let flags = self.flags(&tokens, &project.id, client.as_ref()).await;
        Ok(models::encode(self.app, flags.offered(&quota)))
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let client = self.client(context.network_proxy.as_ref())?;
        let tokens = self
            .live(context.credentials.as_ref(), client.as_ref(), false)
            .await?;
        let project = self.project(&tokens, client.as_ref()).await?;
        let mut value = if self.app == App::Antigravity {
            let models = self
                .antigravity_models(&tokens, &project, client.as_ref(), true)
                .await?;
            let summary = self
                .call(
                    &tokens,
                    "retrieveUserQuotaSummary",
                    &json!({"project":project.id}),
                    client.as_ref(),
                )
                .await
                .ok();
            usage::antigravity(&models, summary.as_ref())?
        } else {
            let quota = self
                .call(
                    &tokens,
                    "retrieveUserQuota",
                    &json!({"project":project.id}),
                    client.as_ref(),
                )
                .await?;
            let flags = self.flags(&tokens, &project.id, client.as_ref()).await;
            usage::gemini(&quota, &flags)?
        };
        value["account"] = profile(&tokens);
        value["account"]["plan"] = json!(project.plan);
        value["account"]["id"] = json!(context.account);
        Ok(value)
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        let url = url::Url::parse(&request.url)
            .map_err(|_| invalid("The Code Assist request address is invalid."))?;
        let base = url::Url::parse(self.app.base()).expect("constant URL");
        if request.method != "POST"
            || url.origin() != base.origin()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid(
                "Code Assist requests must use this application's official endpoint.",
            ));
        }
        let model = url
            .path()
            .strip_prefix("/v1beta/models/")
            .and_then(|path| path.strip_suffix(":streamGenerateContent"))
            .filter(|id| models::valid_id(id))
            .ok_or_else(|| {
                invalid("Code Assist only accepts Google streamGenerateContent requests.")
            })?
            .to_owned();
        if url
            .query_pairs()
            .any(|(key, _)| key != "alt" && key != "key")
        {
            return Err(invalid("The Code Assist request query is invalid."));
        }
        let requested_effort = request
            .metadata
            .get("reasoningEffort")
            .filter(|effort| {
                [
                    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
                ]
                .contains(&effort.as_str())
            })
            .cloned();
        let credentials = request.credentials.clone();
        let proxy = request.network_proxy.clone();
        let body = tokio::task::spawn_blocking(move || {
            let body = request
                .decode_body()?
                .ok_or_else(|| invalid("The Code Assist request body is missing."))?;
            serde_json::from_slice::<Value>(&body)
                .map_err(|_| invalid("The Code Assist request body is not valid JSON."))
        })
        .await
        .map_err(|_| invalid("The Code Assist request body could not be read."))??;
        let client = self.client(proxy.as_ref())?;
        let tokens = self
            .live(credentials.as_ref(), client.as_ref(), false)
            .await?;
        let project = self.project(&tokens, client.as_ref()).await?;
        let sent = if self.app == App::Antigravity {
            let payload = self
                .antigravity_models(&tokens, &project, client.as_ref(), false)
                .await?;
            let catalog = models::antigravity(&payload)?;
            let effort = requested_effort.as_deref().or_else(|| {
                body.pointer("/generationConfig/thinkingConfig/thinkingLevel")
                    .and_then(Value::as_str)
            });
            catalog.wire(&model, effort)
        } else {
            self.flags(&tokens, &project.id, client.as_ref())
                .await
                .wire(&model)
        };
        let system = body
            .pointer("/systemInstruction/parts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        let prompt_restricted = self.app == App::Antigravity
            && (system.contains("x-anthropic-billing-header:")
                || system.contains("built on Anthropic's Claude Agent SDK"));
        let (body, names) = envelope::build_with_effort(
            self.app,
            &sent,
            &body,
            &project.id,
            requested_effort.as_deref(),
        )?;
        let outgoing = HttpRequest::new(
            HttpMethod::Post,
            format!(
                "{}/v1internal:streamGenerateContent?alt=sse",
                self.app.base()
            ),
        )
        .with_header("authorization", format!("Bearer {}", tokens.access))
        .with_header(
            "user-agent",
            self.app.user_agent_with_version(
                &model,
                if tokens.version.is_empty() {
                    "2.9.1"
                } else {
                    &tokens.version
                },
            ),
        )
        .with_json_body(&body)
        .map_err(|_| invalid("The Code Assist request cannot be encoded."))?;
        let response =
            tokio::time::timeout(std::time::Duration::from_secs(60), client.stream(outgoing))
                .await
                .map_err(|_| PluginError::transport("The Code Assist request timed out."))?
                .map_err(|_| {
                    PluginError::transport("The Code Assist service could not be reached.")
                })?;
        stream::response(
            response,
            names,
            vec![tokens.access, tokens.refresh],
            prompt_restricted,
        )
        .await
    }
}
#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;

#[cfg(test)]
#[path = "../test/refresh.test.rs"]
mod refresh_tests;
