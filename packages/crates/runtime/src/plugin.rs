use crate::{auth, definition::Definition, discovery, transport, ProviderAdapter};
use async_trait::async_trait;
use nora_ai::{HttpClient, JsonValue, OAuthCredentials, OAuthError, OAuthProviderId};
use nora_provider_auth::{browser_sign_in::BrowserSignIn, OAuthCallbackInput, OAuthLoginCallbacks};
use nora_provider_plugin_sdk::model_resolution::{ModelResolution, ResolveModelRequest};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, PluginError,
    PluginResult, ProviderPlugin, API_VERSION,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
#[cfg(test)]
use tokio::task::JoinHandle;

#[path = "plugin_configuration.rs"]
mod configuration;
use configuration::Configuration;

enum LoginNotice {
    Url(String),
    Progress(String),
    Callback(OAuthCallbackInput, Option<String>),
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
    configuration: RwLock<Option<Arc<Configuration>>>,
    logins: Mutex<BTreeMap<String, BrowserSignIn<OAuthCredentials>>>,
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

    fn configuration(&self) -> PluginResult<Arc<Configuration>> {
        self.configuration
            .read()
            .map_err(|_| PluginError::invalid("The provider configuration is unavailable."))?
            .clone()
            .ok_or_else(|| PluginError::invalid("Initialize the provider before using it."))
    }

    async fn start_login(
        &self,
        definition: &Definition,
        client: crate::network_client::Client,
        proxy: Option<NetworkProxy>,
        inputs: Value,
    ) -> PluginResult<Value> {
        if definition.config.oauth_provider.is_none() {
            return Err(PluginError::unsupported("oauth"));
        }
        let adapter = definition.adapter.clone();
        let (notice, mut notices) = mpsc::unbounded_channel();
        let progress = notice.clone();
        let callback_notice = notice.clone();
        let callbacks = OAuthLoginCallbacks::new(
            move |url| {
                let _ = notice.send(LoginNotice::Url(url.into()));
            },
            move |message| {
                let _ = progress.send(LoginNotice::Progress(message.into()));
            },
        )
        .with_callback(move |callback, blocked| {
            let _ = callback_notice.send(LoginNotice::Callback(callback, blocked));
        })
        .with_prompt(|_| {
            Box::pin(async {
                Err(OAuthError::InvalidResponse(
                    "This login requires a browser callback.".into(),
                ))
            })
        });
        let mut pending = BrowserSignIn::new(
            tokio::spawn(async move {
                tokio::time::timeout(
                    Duration::from_secs(900),
                    adapter.login_with_inputs(callbacks, inputs, &client),
                )
                .await
                .map_err(|_| PluginError::authentication("The provider sign-in timed out."))?
                .map_err(|_| {
                    PluginError::authentication("The provider sign-in failed. Start again.")
                })
            }),
            None,
        );
        let mut callback_blocked = None;
        let mut instructions = Vec::new();
        let url=tokio::time::timeout(Duration::from_secs(40),async {
            loop {
                tokio::select! {
                    result=&mut pending.task=>{
                        let credentials=result.map_err(|_|PluginError::authentication("The provider sign-in stopped."))?
                            .map_err(|_|PluginError::authentication("The provider could not start this sign-in. Try again."))?;
                        return Ok::<_,PluginError>(Err(credentials));
                    },
                    notice=notices.recv()=>match notice {
                        Some(LoginNotice::Url(url))=>return Ok(Ok(url)),
                        Some(LoginNotice::Progress(message))=>instructions.push(message),
                        Some(LoginNotice::Callback(callback, blocked))=>{pending.callback=Some(callback); callback_blocked=blocked;},
                        None=>return Err(PluginError::authentication("The provider did not return a sign-in address.")),
                    }
                }
            }
        }).await.map_err(|_|PluginError::authentication("The provider sign-in request timed out."))??;
        let url = match url {
            Ok(url) => url,
            Err(credentials) => {
                let client = crate::network_client::Client::new(None, proxy.as_ref(), false)?;
                let credentials = self
                    .enriched_credentials(definition, credentials, &client, proxy.as_ref())
                    .await?;
                return Ok(
                    json!({"ok":true,"credentials":auth::encode(&credentials,definition.config.oauth_provider.as_deref().unwrap_or_default(),false)}),
                );
            }
        };
        while let Ok(notice) = notices.try_recv() {
            match notice {
                LoginNotice::Progress(message) => instructions.push(message),
                LoginNotice::Callback(callback, blocked) => {
                    pending.callback = Some(callback);
                    callback_blocked = blocked;
                }
                LoginNotice::Url(_) => {}
            }
        }
        let session = nora_ai::oauth_pkce::random_hex(24);
        let mut reply = json!({"session":session,"url":url,"method":"auto","instructions":instructions.join("\n")});
        if pending.callback.is_some() {
            reply["pasteCallback"] = json!(true);
            reply["callbackMode"] = json!("poll");
            if let Some(blocked) = callback_blocked {
                reply["callbackBlocked"] = json!(blocked);
                reply["callbackPort"] = json!(1455);
            }
        }
        self.logins.lock().await.insert(session, pending);
        Ok(reply)
    }
}

#[async_trait]
impl ProviderPlugin for BundledProvider {
    fn supports_web_search(&self) -> bool {
        self.adapter
            .as_ref()
            .is_some_and(|adapter| adapter.supports_web_search())
    }

    async fn web_search(&self, context: CallContext, query: String) -> PluginResult<Value> {
        let configuration = self.configuration()?;
        let definition = configuration.account_definition(context.credentials.as_ref())?;
        let proxy = configuration.effective_proxy(context.network_proxy.as_ref())?;
        let client = configuration.request_client(
            &definition,
            proxy.as_ref(),
            context.credentials.as_ref(),
        )?;
        let credentials = context
            .credentials
            .as_ref()
            .ok_or_else(|| PluginError::authentication("Connect a provider account."))?;
        let (_, auth) =
            auth::resolve_with_network(&definition, credentials, proxy.as_ref()).await?;
        definition.adapter.web_search(&auth, &query, &client).await
    }

    fn model_resolution(&self) -> bool {
        self.adapter
            .as_ref()
            .is_some_and(|adapter| adapter.supports_model_resolution())
    }

    async fn resolve_model(
        &self,
        request: ResolveModelRequest,
    ) -> PluginResult<Option<ModelResolution>> {
        let configuration = self.configuration()?;
        let value = request.context.credentials.as_ref().ok_or_else(|| {
            PluginError::authentication("Connect a provider account before selecting a model.")
        })?;
        let definition = configuration.account_definition(Some(value))?;
        if !definition.adapter.supports_model_resolution() {
            return Ok(None);
        }
        let proxy = configuration.effective_proxy(request.context.network_proxy.as_ref())?;
        let client = configuration.request_client(&definition, proxy.as_ref(), Some(value))?;
        let (_, resolved) = auth::resolve_with_network(&definition, value, proxy.as_ref()).await?;
        definition
            .adapter
            .resolve_model(&definition, request, &resolved, &client)
            .await
    }

    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        let configuration = Configuration::parse(params, self.adapter.as_ref())?;
        let account_discovery = configuration
            .definition
            .adapter
            .supports_account_discovery();
        let mut sdk_features = json!({"connectionOverrides":1});
        if configuration
            .definition
            .adapter
            .account_import_kind()
            .is_some()
        {
            sdk_features["accountImport"] = json!(1);
        }
        *self
            .configuration
            .write()
            .map_err(|_| PluginError::invalid("The provider configuration is unavailable."))? =
            Some(Arc::new(configuration));
        Ok(
            json!({"apiVersion":API_VERSION,"capabilities":{"accountDiscovery":account_discovery,"modelMetadata":true,"sdkFeatures":sdk_features}}),
        )
    }

    async fn auth_methods(&self, _context: CallContext) -> PluginResult<Value> {
        Ok(json!(crate::account_headers::methods(
            &self.configuration()?.definition
        )))
    }

    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        let configuration = self.configuration()?;
        let mut definition = configuration.account_definition(context.credentials.as_ref())?;
        definition.request_account = Some(context.account.clone());
        let data_directory = &configuration.data_directory;
        let proxy = configuration.effective_proxy(context.network_proxy.as_ref())?;
        let client = configuration.request_client(
            &definition,
            proxy.as_ref(),
            context.credentials.as_ref(),
        )?;
        discovery::discover_with_network(
            &definition,
            context.credentials.as_ref(),
            data_directory,
            &client,
            proxy.as_ref(),
        )
        .await
    }

    async fn discover_accounts(&self, context: CallContext) -> PluginResult<Value> {
        let configuration = self.configuration()?;
        let definition = configuration.installed_definition.clone();
        let proxy = configuration.effective_proxy(context.network_proxy.as_ref())?;
        let client = configuration.client(proxy.as_ref())?;
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
        let configuration = self.configuration()?;
        let Some(value) = context.credentials.as_ref() else {
            return Ok(json!({"accounts":[]}));
        };
        let definition = configuration.account_definition(Some(value))?;
        let credentials = auth::decode(&definition, Some(value))?;
        let proxy = configuration.effective_proxy(context.network_proxy.as_ref())?;
        let client = configuration.request_client(
            &definition,
            proxy.as_ref(),
            context.credentials.as_ref(),
        )?;
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
        let configuration = self.configuration()?;
        let definition = configuration.definition.clone();
        let proxy = configuration.params_proxy(&params)?;
        let client = configuration.client(proxy.as_ref())?;
        let method = params
            .get("authMethodID")
            .or_else(|| params.get("method"))
            .and_then(Value::as_str)
            .unwrap_or("apiKey");
        let signed_in_definition = configuration.installed_definition.clone();
        if signed_in_definition
            .config
            .source_auth_methods
            .iter()
            .any(|source| source.get("id").and_then(Value::as_str) == Some(method))
        {
            let definition = signed_in_definition;
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
            let inputs = params
                .get("inputs")
                .or_else(|| params.get("fields"))
                .cloned()
                .unwrap_or_else(|| json!({}));
            return self
                .start_login(&signed_in_definition, client, proxy, inputs)
                .await;
        }
        let profile = crate::profiles::by_method(&definition, method);
        if method != "apiKey" && profile.is_none() {
            return Err(PluginError::unsupported(method));
        }
        let optional = profile.map_or(definition.config.key_optional, |profile| {
            profile.key_optional
        });
        let inputs = params
            .get("inputs")
            .or_else(|| params.get("fields"))
            .unwrap_or(&params);
        let supplied = inputs
            .get("apiKey")
            .or_else(|| inputs.get("key"))
            .or_else(|| inputs.get("access"));
        let key = match supplied {
            Some(Value::String(value))
                if !value.contains(['\r', '\n', '\0'])
                    && (optional || !value.trim().is_empty()) =>
            {
                value.trim()
            }
            None if optional => "",
            _ => {
                return Err(PluginError::authentication(
                    "Enter a valid API key for this provider.",
                ))
            }
        };
        let mut credentials = OAuthCredentials::api_key(OAuthProviderId::ApiKey, key);
        definition
            .adapter
            .configure_api_credentials(inputs, &mut credentials)?;
        crate::profiles::assign(&definition, method, inputs, &mut credentials)?;
        crate::account_headers::assign(&definition, inputs, &mut credentials)?;
        credentials
            .extras
            .insert("baseUrl".into(), JsonValue::string(&definition.base_url));
        if let Some(label) = params.get("label").and_then(Value::as_str) {
            credentials
                .extras
                .insert("name".into(), JsonValue::string(label));
        }
        let mut saved = auth::encode(&credentials, &definition.config.key_provider, false);
        if profile.is_some() {
            saved["authMethod"] = json!(method);
        }
        Ok(json!({"ok":true,"credentials":saved}))
    }

    async fn callback(&self, params: Value) -> PluginResult<Value> {
        let session = params
            .get("session")
            .and_then(Value::as_str)
            .ok_or_else(|| PluginError::invalid("The sign-in session is required."))?;
        let mut sessions = self.logins.lock().await;
        let waiting = sessions.get(session).ok_or_else(|| {
            PluginError::authentication("This sign-in session has expired. Start again.")
        })?;
        if let Some(reply) = waiting.pending_reply(session, &params)? {
            return Ok(reply);
        }
        let mut pending = sessions
            .remove(session)
            .expect("The waiting sign-in was checked");
        drop(sessions);
        let credentials = pending.finish().await?;
        let configuration = self.configuration()?;
        let definition = configuration.installed_definition.clone();
        let proxy = configuration.params_proxy(&params)?;
        let client = configuration.client(proxy.as_ref())?;
        let credentials = self
            .enriched_credentials(&definition, credentials, &client, proxy.as_ref())
            .await?;
        Ok(
            json!({"ok":true,"credentials":auth::encode(&credentials,definition.config.oauth_provider.as_deref().unwrap_or_default(),false)}),
        )
    }

    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let configuration = self.configuration()?;
        let definition = configuration.account_definition(params.get("credentials"))?;
        let proxy = configuration.params_proxy(&params)?;
        let client = configuration.client(proxy.as_ref())?;
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
        let configuration = self.configuration()?;
        let definition = configuration.account_definition(context.credentials.as_ref())?;
        let proxy = configuration.effective_proxy(context.network_proxy.as_ref())?;
        let client = configuration.request_client(
            &definition,
            proxy.as_ref(),
            context.credentials.as_ref(),
        )?;
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
        let mut usage = if crate::endpoint_overrides::applies(&definition, value) {
            crate::endpoint_overrides::balance(&definition, value, &resolved, &client).await?
        } else {
            tokio::time::timeout(
                Duration::from_secs(30),
                definition.adapter.usage(&resolved, account, &client),
            )
            .await
            .map_err(|_| PluginError::transport("The provider did not return its usage in time"))?
            .map_err(PluginError::transport)?
        };
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
        let configuration = self.configuration()?;
        let mut definition = configuration.account_definition(request.credentials.as_ref())?;
        definition.request_account = Some(request.account.clone());
        crate::request_metadata::apply(&definition, &mut request)?;
        request.network_proxy = configuration.effective_proxy(request.network_proxy.as_ref())?;
        let client = configuration.request_client(
            &definition,
            request.network_proxy.as_ref(),
            request.credentials.as_ref(),
        )?;
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
        let credentials = request
            .credentials
            .clone()
            .ok_or_else(|| PluginError::authentication("Connect a provider account."))?;
        let original = request.clone();
        let request = transport::build_request(&definition, request).await?;
        let api = transport::request_api(&definition, &credentials, &request.url);
        let secrets = transport::request_secrets(&request);
        let response = transport::send_with_proxy(
            &client,
            &definition,
            &credentials,
            &api,
            request,
            original.network_proxy.as_ref(),
        )
        .await?;
        let response = transport::finish_response(response, secrets).await?;
        definition.adapter.transform_response(&original, response)
    }

    async fn call(&self, method: &str, params: Value) -> PluginResult<Value> {
        let configuration = self.configuration()?;
        if method == "modelMetadata" {
            let directory = &configuration.data_directory;
            return crate::model_metadata::cached(directory, &params);
        }
        if matches!(method, "inspectAccountImport" | "importAccount") {
            let kind = configuration
                .installed_definition
                .adapter
                .account_import_kind()
                .ok_or_else(|| PluginError::unsupported(method))?;
            if method == "inspectAccountImport" {
                return crate::account_import::inspect(kind, &params);
            }
            return crate::account_import::import_login(
                kind,
                &params,
                &configuration.params_client(&params)?,
                &crate::account_import::HostPersistence,
            )
            .await;
        }
        if method != "maintain" {
            return Err(PluginError::unsupported(method));
        }
        let directory = &configuration.data_directory;
        if !directory.is_absolute() {
            return Err(PluginError::invalid(
                "The provider data directory is unavailable.",
            ));
        }
        let client = configuration.params_client(&params)?;
        let before = crate::model_catalog::cached(directory).ok();
        let after = crate::model_catalog::load(&client, directory).await?;
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

#[cfg(test)]
#[path = "../test/mixed_accounts.test.rs"]
mod mixed_accounts_tests;

#[cfg(test)]
#[path = "../test/profile_connections.test.rs"]
mod profile_connection_tests;

#[cfg(test)]
#[path = "../test/callback_paste.test.rs"]
mod callback_paste_tests;
