use crate::{auth, definition::Definition, discovery, transport, ProviderAdapter};
use async_trait::async_trait;
use nora_ai::{
    HttpClient, JsonValue, OAuthCredentials, OAuthError, OAuthProviderId, ReqwestHttpClient,
};
use nora_provider_auth::OAuthLoginCallbacks;
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, PluginError, PluginResult,
    ProviderPlugin, API_VERSION,
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

#[derive(Default)]
pub struct BundledProvider {
    adapter: Option<Arc<dyn ProviderAdapter>>,
    definition: RwLock<Option<Definition>>,
    data_directory: RwLock<PathBuf>,
    logins: Mutex<BTreeMap<String, PendingLogin>>,
}

impl BundledProvider {
    pub fn new(adapter: impl ProviderAdapter) -> Self {
        Self {
            adapter: Some(Arc::new(adapter)),
            ..Self::default()
        }
    }

    fn definition(&self) -> PluginResult<Definition> {
        self.definition
            .read()
            .map_err(|_| PluginError::invalid("The provider configuration is unavailable."))?
            .clone()
            .ok_or_else(|| PluginError::invalid("Initialize the provider before using it."))
    }

    async fn start_login(&self, definition: &Definition) -> PluginResult<Value> {
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
            let client = ReqwestHttpClient::new();
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
                return Ok(
                    json!({"ok":true,"credentials":auth::encode(&credentials,definition.config.oauth_provider.as_deref().unwrap_or_default(),false)}),
                )
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
        *self
            .definition
            .write()
            .map_err(|_| PluginError::invalid("The provider configuration is unavailable."))? =
            Some(definition);
        Ok(json!({"apiVersion":API_VERSION}))
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
        discovery::discover(
            &definition,
            context.credentials.as_ref(),
            &data_directory,
            &ReqwestHttpClient::new(),
        )
        .await
    }

    async fn accounts(&self, context: CallContext) -> PluginResult<Value> {
        let Some(value) = context.credentials.as_ref() else {
            return Ok(json!([]));
        };
        let credentials = auth::decode(&self.definition()?, Some(value))?;
        let mut profile = auth::public_profile(&credentials);
        profile["id"] = json!(context.account);
        Ok(json!([profile]))
    }

    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        let definition = self.definition()?;
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
            let credentials = definition.adapter.authorize_source(method).await?;
            return Ok(
                json!({"ok":true,"credentials":auth::encode(&credentials,definition.config.oauth_provider.as_deref().unwrap_or(""),true)}),
            );
        }
        if method == "oauth" {
            return self.start_login(&definition).await;
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
        Ok(
            json!({"ok":true,"credentials":auth::encode(&credentials,definition.config.oauth_provider.as_deref().unwrap_or_default(),false)}),
        )
    }

    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let definition = self.definition()?;
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
        let credentials = definition
            .adapter
            .refresh(&credentials, &ReqwestHttpClient::new())
            .await?;
        Ok(json!({"credentials":auth::encode(&credentials,provider_id,false)}))
    }

    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let definition = self.definition()?;
        let value = context
            .credentials
            .as_ref()
            .ok_or_else(|| PluginError::authentication("Connect a provider account."))?;
        let (credentials, resolved) = auth::resolve(&definition, value).await?;
        let account = credentials
            .extras
            .get("accountId")
            .or_else(|| credentials.extras.get("userId"))
            .and_then(|value| value.as_str());
        let mut usage = tokio::time::timeout(
            Duration::from_secs(30),
            definition
                .adapter
                .usage(&resolved, account, &ReqwestHttpClient::new()),
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

    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        let definition = self.definition()?;
        let request = transport::build_request(&definition, request).await?;
        let secrets = transport::request_secrets(&request);
        let response = ReqwestHttpClient::new()
            .stream(request)
            .await
            .map_err(|_| PluginError::transport("The provider endpoint could not be reached."))?;
        transport::finish_response(response, secrets).await
    }

    async fn call(&self, method: &str, _params: Value) -> PluginResult<Value> {
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
        let before = crate::model_catalog::cached(&directory).ok();
        let after = crate::model_catalog::load(&ReqwestHttpClient::new(), &directory).await?;
        Ok(json!({"ok":true,"modelsChanged":before.as_ref()!=Some(&after)}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn provider() -> BundledProvider {
        let provider = BundledProvider::new(crate::openai_provider::OpenAI);
        provider
            .initialize(InitializeParams {
                api_version: API_VERSION,
                plugin_id: "openai".into(),
                plugin_directory: std::path::PathBuf::new(),
                data_directory: std::path::PathBuf::new(),
                manifest: serde_json::from_slice(
                    &std::fs::read(
                        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                            .parent()
                            .unwrap()
                            .join("openai/nora-provider.json"),
                    )
                    .unwrap(),
                )
                .unwrap(),
            })
            .await
            .unwrap();
        provider
    }

    #[tokio::test]
    async fn native_provider_rejects_a_different_providers_manifest() {
        let provider = BundledProvider::new(crate::xai_provider::Xai);
        let result = provider
            .initialize(InitializeParams {
                api_version: API_VERSION,
                plugin_id: "openai".into(),
                plugin_directory: PathBuf::new(),
                data_directory: PathBuf::new(),
                manifest: serde_json::from_str(include_str!("../../openai/nora-provider.json"))
                    .unwrap(),
            })
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn maintenance_finishes_against_a_valid_local_catalog_without_account_credentials() {
        let provider = provider().await;
        let directory =
            std::env::temp_dir().join(format!("nora-provider-maintain-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("models.json"),serde_json::to_vec(&json!({"vendor":{"id":"vendor","models":{"fresh-model":{"id":"fresh-model","modalities":{"output":["text"]},"limit":{"context":100000,"output":10000}}}}})).unwrap()).unwrap();
        *provider.data_directory.write().unwrap() = directory.clone();
        let result = provider
            .call(
                "maintain",
                json!({"credentials":{"access":"invalid-value-must-not-be-used"}}),
            )
            .await
            .unwrap();
        assert_eq!(result, json!({"ok":true,"modelsChanged":false}));
        assert!(provider.call("unknown", json!({})).await.is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn api_authorization_returns_a_readable_separate_account_value() {
        let provider = provider().await;
        let first = provider
            .authorize(json!({"method":"apiKey","inputs":{"apiKey":"first-private"}}))
            .await
            .unwrap();
        let second = provider
            .authorize(json!({"method":"apiKey","label":"Work","fields":{"key":"second-private"}}))
            .await
            .unwrap();
        assert_eq!(first["credentials"]["label"], "API key");
        assert_eq!(second["credentials"]["label"], "Work");
        assert_eq!(first["credentials"]["access"], "first-private");
        assert_eq!(second["credentials"]["access"], "second-private");
        assert_eq!(second["credentials"]["type"], "apiKey");
        assert!(provider
            .authorize(json!({"method":"apiKey","inputs":{"apiKey":"bad\nkey"}}))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn api_account_refresh_does_not_attempt_an_oauth_login() {
        let provider = provider().await;
        let value = provider
            .authorize(json!({"method":"apiKey","apiKey":"private-key"}))
            .await
            .unwrap();
        let refreshed = provider
            .refresh(json!({"credentials":value["credentials"],"force":true}))
            .await
            .unwrap();
        assert_eq!(refreshed["changed"], false);
    }
    #[tokio::test]
    async fn saved_codex_accounts_derive_a_safe_profile_without_signing_in_again() {
        use base64::Engine;
        let provider = provider().await;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"workspace-id","chatgpt_plan_type":"plus"},"https://api.openai.com/profile":{"email":"historic@example.com","name":"Historic Name"}}"#);
        let credentials = json!({"type":"oauth","credentialProvider":"openAICodex","access":format!("header.{payload}.signature"),"refresh":"private-refresh"});
        let accounts = provider
            .accounts(CallContext {
                account: "work".into(),
                credentials: Some(credentials),
            })
            .await
            .unwrap();
        assert_eq!(
            accounts[0],
            json!({"id":"work","email":"historic@example.com","name":"Historic Name","plan":"plus"})
        );
        let visible = accounts.to_string();
        assert!(!visible.contains("private-refresh"));
        assert!(!visible.contains("signature"));
        assert!(!visible.contains("workspace-id"));
    }

    #[tokio::test]
    async fn api_account_usage_exposes_its_profile_without_requiring_a_usage_endpoint() {
        let provider = provider().await;
        let usage = provider.usage(CallContext {
            account:"personal".into(),
            credentials:Some(json!({"type":"apiKey","access":"private-api-secret","extras":{"email":"api@example.com","name":"API Member","plan":"team"}})),
        }).await.unwrap();
        assert_eq!(usage["account"]["email"], "api@example.com");
        assert_eq!(usage["account"]["id"], "personal");
        assert_eq!(usage["account"]["plan"], "team");
        assert!(!usage.to_string().contains("private-api-secret"));
    }
}
