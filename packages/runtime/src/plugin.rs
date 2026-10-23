use crate::{auth, definition::Definition, discovery, transport, ProviderAdapter};
use async_trait::async_trait;
use nora_ai::{
    HttpClient, JsonValue, OAuthCredentials, OAuthError, OAuthProviderId, ReqwestHttpClient,
};
use nora_provider_auth::OAuthLoginCallbacks;
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, PluginError,
    PluginResult, ProviderPlugin, API_VERSION,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

struct PendingLogin(JoinHandle<Result<OAuthCredentials, OAuthError>>);
impl Drop for PendingLogin {
    fn drop(&mut self) {
        self.0.abort();
    }
}

enum LoginNotice {
    Url(String),
    Progress(String),
}

fn valid_account_identity(identity: &Value) -> bool {
    let text = |key: &str, max: usize, required: bool| match identity.get(key) {
        None => !required,
        Some(Value::String(value)) => {
            (!required || !value.is_empty())
                && value.len() <= max
                && !value.chars().any(char::is_control)
        }
        Some(_) => false,
    };
    identity.is_object()
        && text("issuer", 256, true)
        && text("subject", 512, true)
        && text("scope", 512, false)
}

#[derive(Default)]
pub struct BundledProvider {
    adapter: Option<Arc<dyn ProviderAdapter>>,
    definition: RwLock<Option<Definition>>,
    data_directory: RwLock<PathBuf>,
    network_proxy: RwLock<Option<NetworkProxy>>,
    logins: Mutex<BTreeMap<String, PendingLogin>>,
}

impl BundledProvider {
    pub fn new(adapter: impl ProviderAdapter) -> Self {
        Self {
            adapter: Some(Arc::new(adapter)),
            ..Self::default()
        }
    }

    async fn enriched_credentials(
        &self,
        definition: &Definition,
        mut credentials: OAuthCredentials,
        client: &dyn HttpClient,
        proxy: Option<&NetworkProxy>,
    ) -> PluginResult<OAuthCredentials> {
        definition
            .adapter
            .enrich_account(&mut credentials, client, proxy)
            .await?;
        Ok(credentials)
    }

    fn definition(&self) -> PluginResult<Definition> {
        self.definition
            .read()
            .map_err(|_| PluginError::invalid("The provider configuration is unavailable."))?
            .clone()
            .ok_or_else(|| PluginError::invalid("Initialize the provider before using it."))
    }

    fn effective_proxy(&self, proxy: Option<&NetworkProxy>) -> PluginResult<Option<NetworkProxy>> {
        let default = self
            .network_proxy
            .read()
            .map_err(|_| PluginError::invalid("The network proxy configuration is unavailable."))?;
        let proxy = proxy.or(default.as_ref()).cloned();
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        Ok(proxy)
    }

    fn client(&self, proxy: Option<&NetworkProxy>) -> PluginResult<ReqwestHttpClient> {
        let proxy = self.effective_proxy(proxy)?;
        nora_provider_plugin_sdk::http_client(proxy.as_ref()).map(|client| {
            ReqwestHttpClient::with_client(client).with_observer_fn(|| async {
                nora_provider_plugin_sdk::before_http_request()
                    .await
                    .map_err(|error| nora_ai::HttpClientError::Transport(error.to_string()))
            })
        })
    }

    fn params_proxy(&self, params: &Value) -> PluginResult<Option<NetworkProxy>> {
        let proxy: Option<NetworkProxy> =
            serde_json::from_value(params.get("networkProxy").cloned().unwrap_or(Value::Null))
                .map_err(|_| PluginError::invalid("The network proxy configuration is invalid."))?;
        self.effective_proxy(proxy.as_ref())
    }

    fn params_client(&self, params: &Value) -> PluginResult<ReqwestHttpClient> {
        self.client(self.params_proxy(params)?.as_ref())
    }

    async fn start_login(
        &self,
        definition: &Definition,
        client: ReqwestHttpClient,
        proxy: Option<NetworkProxy>,
    ) -> PluginResult<Value> {
        if definition.config.oauth_provider.is_none() {
            return Err(PluginError::unsupported("oauth"));
        }
        let adapter = definition.adapter.clone();
        let (notice, mut notices) = mpsc::unbounded_channel();
        let progress = notice.clone();
        let callbacks = OAuthLoginCallbacks::new(
            move |url| {
                let _ = notice.send(LoginNotice::Url(url.into()));
            },
            move |message| {
                let _ = progress.send(LoginNotice::Progress(message.into()));
            },
        )
        .with_prompt(|_| {
            Box::pin(async {
                Err(OAuthError::InvalidResponse(
                    "This login requires a browser callback.".into(),
                ))
            })
        });
        let mut pending = PendingLogin(tokio::spawn(async move {
            adapter.login(callbacks, &client).await
        }));
        let mut instructions = Vec::new();
        let url=tokio::time::timeout(Duration::from_secs(40),async {
            loop {
                tokio::select! {
                    result=&mut pending.0=>{
                        let credentials=result.map_err(|_|PluginError::authentication("The provider sign-in stopped."))?
                            .map_err(|_|PluginError::authentication("The provider could not start this sign-in. Try again."))?;
                        return Ok::<_,PluginError>(Err(credentials));
                    },
                    notice=notices.recv()=>match notice {
                        Some(LoginNotice::Url(url))=>return Ok(Ok(url)),
                        Some(LoginNotice::Progress(message))=>instructions.push(message),
                        None=>return Err(PluginError::authentication("The provider did not return a sign-in address.")),
                    }
                }
            }
        }).await.map_err(|_|PluginError::authentication("The provider sign-in request timed out."))??;
        let url = match url {
            Ok(url) => url,
            Err(credentials) => {
                let client = self.client(proxy.as_ref())?;
                let credentials = self
                    .enriched_credentials(definition, credentials, &client, proxy.as_ref())
                    .await?;
                return Ok(
                    json!({"ok":true,"credentials":auth::encode(&credentials,definition.config.oauth_provider.as_deref().unwrap_or_default(),false)}),
                );
            }
        };
        while let Ok(notice) = notices.try_recv() {
            if let LoginNotice::Progress(message) = notice {
                instructions.push(message);
            }
        }
        let session = "oauth".to_string();
        self.logins.lock().await.insert(session.clone(), pending);
        Ok(
            json!({"session":session,"url":url,"method":"auto","instructions":instructions.join("\n")}),
        )
    }
}

#[async_trait]
impl ProviderPlugin for BundledProvider {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        if params.api_version != API_VERSION {
            return Err(PluginError::invalid(
                "Unsupported provider plugin API version.",
            ));
        }
        let proxy: Option<NetworkProxy> = serde_json::from_value(
            params
                .manifest
                .pointer("/config/networkProxy")
                .cloned()
                .unwrap_or(Value::Null),
        )
        .map_err(|_| PluginError::invalid("The network proxy configuration is invalid."))?;
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        *self.network_proxy.write().map_err(|_| {
            PluginError::invalid("The network proxy configuration is unavailable.")
        })? = proxy;
        let mut definition = Definition::parse(params.manifest)?;
        if let Some(adapter) = &self.adapter {
            if adapter.id().is_some_and(|id| id != definition.id) {
                return Err(PluginError::invalid(
                    "The provider executable does not match its manifest.",
                ));
            }
            definition.adapter = adapter.clone();
        }
        *self
            .data_directory
            .write()
            .map_err(|_| PluginError::invalid("The provider data directory is unavailable."))? =
            params.data_directory;
        let account_discovery = definition.adapter.supports_account_discovery();
        *self
            .definition
            .write()
            .map_err(|_| PluginError::invalid("The provider configuration is unavailable."))? =
            Some(definition);
        Ok(json!({"apiVersion":API_VERSION,"capabilities":{"accountDiscovery":account_discovery}}))
    }

    async fn auth_methods(&self, _context: CallContext) -> PluginResult<Value> {
        Ok(json!(self.definition()?.auth_methods))
    }

    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        let definition = self.definition()?;
        let data_directory = self
            .data_directory
            .read()
            .map_err(|_| PluginError::invalid("The provider data directory is unavailable."))?
            .clone();
        let proxy = self.effective_proxy(context.network_proxy.as_ref())?;
        let client = self.client(proxy.as_ref())?;
        discovery::discover_with_network(
            &definition,
            context.credentials.as_ref(),
            &data_directory,
            &client,
            proxy.as_ref(),
        )
        .await
    }

    async fn discover_accounts(&self, context: CallContext) -> PluginResult<Value> {
        let definition = self.definition()?;
        let proxy = self.effective_proxy(context.network_proxy.as_ref())?;
        let client = self.client(proxy.as_ref())?;
        let found = definition
            .adapter
            .discover_accounts(&client, proxy.as_ref())
            .await?;
        if found.len() > 128 {
            return Err(PluginError::invalid(
                "The provider returned too many local accounts.",
            ));
        }
        let mut accounts = Vec::with_capacity(found.len());
        for credentials in found {
            let credentials = self
                .enriched_credentials(&definition, credentials, &client, proxy.as_ref())
                .await?;
            let Some(source) = credentials.extras.get("noraAccountSource") else {
                return Err(PluginError::invalid(
                    "A discovered account must stay bound to its local sign-in.",
                ));
            };
            let Some(source_id) = source
                .as_value()
                .get("kind")
                .and_then(Value::as_str)
                .filter(|id| {
                    !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control)
                })
            else {
                return Err(PluginError::invalid(
                    "A discovered account has an invalid local sign-in source.",
                ));
            };
            if !definition.adapter.is_source(&credentials)
                || !credentials
                    .extras
                    .get("accountIdentity")
                    .is_some_and(|identity| valid_account_identity(identity.as_value()))
            {
                return Err(PluginError::invalid(
                    "A discovered account must provide a verified account identity.",
                ));
            }
            let saved = auth::encode(
                &credentials,
                definition
                    .config
                    .oauth_provider
                    .as_deref()
                    .unwrap_or_default(),
                true,
            );
            accounts.push(json!({"sourceID":source_id,"credentials":saved}));
        }
        Ok(json!({"accounts":accounts}))
    }

    async fn accounts(&self, context: CallContext) -> PluginResult<Value> {
        let Some(value) = context.credentials.as_ref() else {
            return Ok(json!({"accounts":[]}));
        };
        let definition = self.definition()?;
        let credentials = auth::decode(&definition, Some(value))?;
        let proxy = self.effective_proxy(context.network_proxy.as_ref())?;
        let client = self.client(proxy.as_ref())?;
        let credentials = self
            .enriched_credentials(&definition, credentials, &client, proxy.as_ref())
            .await?;
        let mut profile = auth::public_profile(&credentials);
        profile["id"] = json!(context.account);
        let bound = definition.adapter.is_source(&credentials);
        let mut saved = auth::encode(&credentials, auth::provider_id(&definition, value), bound);
        // Account inspection enriches identity and profile information; it does
        // not switch an explicitly chosen plan's authentication method.
        if let Some(method) = value.get("authMethod").filter(|value| value.is_string()) {
            saved["authMethod"] = method.clone();
        }
        let mut result = json!({"accounts":[profile]});
        if saved != *value {
            result["credentials"] = saved;
        }
        Ok(result)
    }

    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        let definition = self.definition()?;
        let proxy = self.params_proxy(&params)?;
        let client = self.client(proxy.as_ref())?;
        let method = params
            .get("authMethodID")
            .or_else(|| params.get("method"))
            .and_then(Value::as_str)
            .unwrap_or("apiKey");
        if definition
            .config
            .source_auth_methods
            .iter()
            .any(|source| source.get("id").and_then(Value::as_str) == Some(method))
        {
            let credentials = definition
                .adapter
                .authorize_source(method, &client, proxy.as_ref())
                .await?;
            let credentials = self
                .enriched_credentials(&definition, credentials, &client, proxy.as_ref())
                .await?;
            // A sign-in read from another app either stays bound to it, and
            // keeps no tokens here, or is taken over like any other sign-in.
            let bound = definition.adapter.is_source(&credentials);
            return Ok(
                json!({"ok":true,"credentials":auth::encode(&credentials,definition.config.oauth_provider.as_deref().unwrap_or(""),bound)}),
            );
        }
        if method == "oauth" {
            return self.start_login(&definition, client, proxy).await;
        }
        if method != "apiKey" {
            return Err(PluginError::unsupported(method));
        }
        let inputs = params
            .get("inputs")
            .or_else(|| params.get("fields"))
            .unwrap_or(&params);
        let key = inputs
            .get("apiKey")
            .or_else(|| inputs.get("key"))
            .or_else(|| inputs.get("access"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty() && !value.contains(['\r', '\n']))
            .ok_or_else(|| PluginError::authentication("Enter an API key for this provider."))?;
        let mut credentials = OAuthCredentials::api_key(OAuthProviderId::ApiKey, key);
        definition
            .adapter
            .configure_api_credentials(inputs, &mut credentials)?;
        credentials
            .extras
            .insert("baseUrl".into(), JsonValue::string(&definition.base_url));
        if let Some(label) = params.get("label").and_then(Value::as_str) {
            credentials
                .extras
                .insert("name".into(), JsonValue::string(label));
        }
        Ok(
            json!({"ok":true,"credentials":auth::encode(&credentials,&definition.config.key_provider,false)}),
        )
    }

    async fn callback(&self, params: Value) -> PluginResult<Value> {
        let session = params
            .get("session")
            .and_then(Value::as_str)
            .ok_or_else(|| PluginError::invalid("The sign-in session is required."))?;
        let mut pending = self.logins.lock().await.remove(session).ok_or_else(|| {
            PluginError::authentication("This sign-in session has expired. Start again.")
        })?;
        let credentials = (&mut pending.0)
            .await
            .map_err(|_| PluginError::authentication("The sign-in was interrupted."))?
            .map_err(|_| {
                PluginError::authentication("The provider sign-in failed. Start again.")
            })?;
        let definition = self.definition()?;
        let proxy = self.params_proxy(&params)?;
        let client = self.client(proxy.as_ref())?;
        let credentials = self
            .enriched_credentials(&definition, credentials, &client, proxy.as_ref())
            .await?;
        Ok(
            json!({"ok":true,"credentials":auth::encode(&credentials,definition.config.oauth_provider.as_deref().unwrap_or_default(),false)}),
        )
    }

    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let definition = self.definition()?;
        let proxy = self.params_proxy(&params)?;
        let client = self.client(proxy.as_ref())?;
        let value = params
            .get("credentials")
            .ok_or_else(|| PluginError::authentication("Connect a provider account."))?;
        let credentials = auth::decode(&definition, Some(value))?;
        if definition.adapter.is_source(&credentials) {
            definition
                .adapter
                .refresh_source(
                    &credentials,
                    params.get("force").and_then(Value::as_bool) == Some(true),
                    proxy.as_ref(),
                )
                .await?;
            return Ok(
                json!({"credentials":auth::encode(&credentials,definition.config.oauth_provider.as_deref().unwrap_or_default(),true)}),
            );
        }
        if auth::is_api(value) {
            return Ok(json!({"changed":false}));
        }
        let provider_id = auth::provider_id(&definition, value);
        let credentials = definition.adapter.refresh(&credentials, &client).await?;
        let credentials = self
            .enriched_credentials(&definition, credentials, &client, proxy.as_ref())
            .await?;
        Ok(json!({"credentials":auth::encode(&credentials,provider_id,false)}))
    }

    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let definition = self.definition()?;
        let proxy = self.effective_proxy(context.network_proxy.as_ref())?;
        let client = self.client(proxy.as_ref())?;
        let value = context
            .credentials
            .as_ref()
            .ok_or_else(|| PluginError::authentication("Connect a provider account."))?;
        let (credentials, resolved) =
            auth::resolve_with_network(&definition, value, proxy.as_ref()).await?;
        let account = credentials
            .extras
            .get("accountId")
            .or_else(|| credentials.extras.get("userId"))
            .and_then(|value| value.as_str());
        let mut usage = tokio::time::timeout(
            Duration::from_secs(30),
            definition.adapter.usage(&resolved, account, &client),
        )
        .await
        .map_err(|_| PluginError::transport("The provider did not return its usage in time"))?
        .map_err(PluginError::transport)?;
        if let Some(value) = usage.as_object_mut() {
            let profile = value.entry("account").or_insert_with(|| json!({}));
            if !profile.is_object() {
                *profile = json!({});
            }
            if let Some(profile) = profile.as_object_mut() {
                if let Some(fallback) = auth::public_profile(&credentials).as_object() {
                    for (key, value) in fallback {
                        if profile
                            .get(key)
                            .and_then(Value::as_str)
                            .is_none_or(|value| value.trim().is_empty())
                        {
                            profile.insert(key.clone(), value.clone());
                        }
                    }
                }
                profile.insert("id".into(), json!(context.account));
            }
        }
        Ok(usage)
    }

    async fn fetch(&self, mut request: FetchRequest) -> PluginResult<FetchResponse> {
        let definition = self.definition()?;
        request.network_proxy = self.effective_proxy(request.network_proxy.as_ref())?;
        let client = self.client(request.network_proxy.as_ref())?;
        if let Some(response) = definition
            .adapter
            .fetch_override(&definition, &request, &client)
            .await?
        {
            return Ok(response);
        }
        if request.body_file.is_some()
            && request.headers.iter().any(|(key, value)| {
                key.eq_ignore_ascii_case("content-type")
                    && value
                        .to_ascii_lowercase()
                        .starts_with("multipart/form-data")
            })
        {
            return crate::transport_file::fetch(&definition, request).await;
        }
        let request = transport::build_request(&definition, request).await?;
        let secrets = transport::request_secrets(&request);
        let response = client
            .stream(request)
            .await
            .map_err(|_| PluginError::transport("The provider endpoint could not be reached."))?;
        transport::finish_response(response, secrets).await
    }

    async fn call(&self, method: &str, params: Value) -> PluginResult<Value> {
        if method != "maintain" {
            return Err(PluginError::unsupported(method));
        }
        let directory = self
            .data_directory
            .read()
            .map_err(|_| PluginError::invalid("The provider data directory is unavailable."))?
            .clone();
        if !directory.is_absolute() {
            return Err(PluginError::invalid(
                "The provider data directory is unavailable.",
            ));
        }
        let client = self.params_client(&params)?;
        let before = crate::model_catalog::cached(&directory).ok();
        let after = crate::model_catalog::load(&client, &directory).await?;
        Ok(json!({"ok":true,"modelsChanged":before.as_ref()!=Some(&after)}))
    }
}

#[cfg(test)]
#[path = "../test/plugin.test.rs"]
mod tests;

#[cfg(test)]
#[path = "../test/plugin_network.test.rs"]
mod network_tests;

#[cfg(test)]
#[path = "../test/account_discovery.test.rs"]
mod account_discovery_tests;
