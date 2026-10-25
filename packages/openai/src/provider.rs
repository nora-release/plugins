#[path = "fast.rs"]
pub(crate) mod fast;

use async_trait::async_trait;
use nora_ai::{
    HttpClient, Model, ModelsCatalog, OAuthCredentials, OAuthError, OAuthProvider, OAuthProviderId,
    ResolvedProviderAuth,
};
use nora_provider::{
    adapter::{is_oauth, merge_headers},
    definition::Definition,
    discovery::{parsing, Kind},
    models, ProviderAdapter,
};
use nora_provider_auth::{codex_source, OAuthLogin, OAuthLoginCallbacks, OpenAICodexOAuthProvider};
use nora_provider_plugin_sdk::{NetworkProxy, PluginError, PluginResult};
use serde_json::Value;
use std::collections::BTreeMap;
use url::Url;

#[path = "codex_version.rs"]
pub mod codex_version;
#[path = "usage.rs"]
pub mod usage;

#[path = "media.rs"]
mod media;

#[derive(Debug)]
pub struct OpenAI;

#[async_trait]
impl ProviderAdapter for OpenAI {
    fn id(&self) -> Option<&'static str> {
        Some("openai")
    }
    fn account_import_kind(&self) -> Option<nora_provider::account_import::LoginKind> {
        Some(nora_provider::account_import::LoginKind::Codex)
    }
    async fn login(
        &self,
        callbacks: OAuthLoginCallbacks,
        client: &dyn HttpClient,
    ) -> Result<OAuthCredentials, OAuthError> {
        OAuthLogin::login_openai_codex_with_originator(callbacks, client, "nora").await
    }
    async fn refresh(
        &self,
        credentials: &OAuthCredentials,
        client: &dyn HttpClient,
    ) -> PluginResult<OAuthCredentials> {
        OpenAICodexOAuthProvider::default()
            .refresh(credentials, client)
            .await
            .map_err(|_| {
                PluginError::authentication(
                    "The provider account could not be renewed. Sign in again.",
                )
            })
    }
    fn enrich_credentials(&self, value: &Value, credentials: &mut OAuthCredentials) {
        if credentials.provider != OAuthProviderId::OpenAICodex {
            return;
        }
        let id_token = value
            .get("idToken")
            .or_else(|| value.get("id_token"))
            .or_else(|| value.pointer("/extras/idToken"))
            .or_else(|| value.pointer("/extras/id_token"))
            .and_then(Value::as_str);
        for (key, value) in OpenAICodexOAuthProvider::profile_extras_from_tokens(
            &credentials.access_token,
            id_token,
        ) {
            if credentials
                .extras
                .get(&key)
                .and_then(|value| value.as_str())
                .is_none_or(|value| value.trim().is_empty())
            {
                credentials.extras.insert(key, value);
            }
        }
    }
    fn supports_account_discovery(&self) -> bool {
        true
    }
    async fn discover_accounts(
        &self,
        _client: &dyn HttpClient,
        _proxy: Option<&NetworkProxy>,
    ) -> PluginResult<Vec<OAuthCredentials>> {
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .filter(|home| home.is_absolute())
            .ok_or_else(|| PluginError::authentication("The home directory is unavailable."))?;
        let path = codex_source::auth_file(&home, &std::env::vars().collect());
        match codex_source::bind(&path) {
            Ok(credentials) => Ok(vec![credentials]),
            Err(codex_source::SourceError::NotSignedIn) => Ok(Vec::new()),
            Err(error) => Err(source_error(error)),
        }
    }
    fn is_source(&self, credentials: &OAuthCredentials) -> bool {
        codex_source::is_bound(credentials)
    }
    async fn authorize_source(
        &self,
        method: &str,
        client: &dyn HttpClient,
        _proxy: Option<&NetworkProxy>,
    ) -> PluginResult<OAuthCredentials> {
        if method != codex_source::KIND {
            return Err(PluginError::unsupported(method));
        }
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .filter(|home| home.is_absolute())
            .ok_or_else(|| PluginError::authentication("The home directory is unavailable."))?;
        let path = codex_source::auth_file(&home, &std::env::vars().collect());
        let credentials = codex_source::bind(&path).map_err(source_error)?;
        // A sign-in Nora cannot use fails here, not on the first request.
        codex_source::resolve(&credentials, false, client)
            .await
            .map_err(source_error)?;
        Ok(credentials)
    }
    async fn refresh_source(
        &self,
        credentials: &OAuthCredentials,
        force: bool,
        proxy: Option<&NetworkProxy>,
    ) -> PluginResult<()> {
        codex_source::resolve(credentials, force, &source_client(proxy)?)
            .await
            .map_err(source_error)?;
        Ok(())
    }
    async fn resolve_source(
        &self,
        credentials: &OAuthCredentials,
        proxy: Option<&NetworkProxy>,
    ) -> PluginResult<ResolvedProviderAuth> {
        let current = codex_source::resolve(credentials, false, &source_client(proxy)?)
            .await
            .map_err(source_error)?;
        let mut auth = nora_provider::auth::base_auth("openai-codex", &current);
        self.enrich_auth(&current, &mut auth);
        Ok(auth)
    }
    fn enrich_auth(&self, credentials: &OAuthCredentials, auth: &mut ResolvedProviderAuth) {
        if credentials.provider != OAuthProviderId::OpenAICodex {
            return;
        }
        auth.base_url = Some("https://chatgpt.com".into());
        auth.headers
            .insert("openai-beta".into(), "responses=experimental".into());
        auth.headers.insert("originator".into(), "nora".into());
        if let Some(account) = credentials
            .extras
            .get("accountId")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .or_else(|| {
                OpenAICodexOAuthProvider::extract_account_id_from_jwt(&credentials.access_token)
            })
        {
            auth.headers.insert("chatgpt-account-id".into(), account);
        }
    }
    fn endpoint(&self, credentials: Option<&Value>) -> Option<String> {
        is_oauth(credentials).then(|| "https://chatgpt.com".into())
    }
    fn catalog<'a>(&self, credentials: Option<&Value>, default: &'a str) -> &'a str {
        if is_oauth(credentials) {
            "openai-codex"
        } else {
            default
        }
    }
    fn model_api(&self, credentials: Option<&Value>, _native_api: &str) -> Option<String> {
        is_oauth(credentials).then(|| "chatgpt-codex".into())
    }
    fn fallback_models(
        &self,
        definition: &Definition,
        credentials: Option<&Value>,
        catalog: &ModelsCatalog,
    ) -> Option<Vec<Model>> {
        if !is_oauth(credentials) {
            return None;
        }
        Some(cli_models(definition, credentials.unwrap(), catalog))
    }
    async fn model_urls(&self, base: &str, credentials: &Value) -> PluginResult<Option<Vec<Url>>> {
        if !is_oauth(Some(credentials)) {
            return Ok(None);
        }
        model_urls_with_version(base, &codex_version::current().await).map(Some)
    }
    fn model_list_headers(
        &self,
        url: &Url,
        auth: &ResolvedProviderAuth,
        headers: &mut BTreeMap<String, String>,
    ) {
        if auth.credential_provider_id.as_deref() != Some("openai-codex")
            && !url.query_pairs().any(|(key, _)| key == "client_version")
        {
            return;
        }
        let version = url
            .query_pairs()
            .find(|(name, _)| name == "client_version")
            .map(|(_, value)| value.into_owned())
            .unwrap_or_else(|| codex_version::MINIMUM.into());
        merge_headers(headers, codex_version::headers(&version));
    }
    fn bearer_model_list(&self, auth: &ResolvedProviderAuth) -> bool {
        auth.credential_provider_id.as_deref() == Some("openai-codex")
    }
    fn model_id<'a>(&self, entry: &'a Value, credentials: &Value) -> Option<&'a str> {
        is_oauth(Some(credentials))
            .then(|| {
                entry
                    .get("slug")
                    .or_else(|| entry.get("id"))
                    .and_then(Value::as_str)
            })
            .flatten()
    }
    fn model_visible(&self, entry: &Value, _id: &str, credentials: &Value) -> bool {
        !is_oauth(Some(credentials))
            || entry.get("visibility").and_then(Value::as_str) != Some("hide")
    }
    fn enrich_model(&self, entry: &Value, model: &mut Model, credentials: &Value) {
        if !is_oauth(Some(credentials)) {
            return;
        }
        let ultrafast = entry
            .get("service_tiers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|tier| {
                tier.as_str()
                    .or_else(|| tier.get("id").and_then(Value::as_str))
                    == Some("ultrafast")
            });
        if !ultrafast {
            model.info.service_tiers.retain(|tier| tier != "ultrafast");
        }
    }
    fn model_priority(&self, entry: &Value, credentials: &Value) -> i64 {
        if is_oauth(Some(credentials)) {
            entry.get("priority").and_then(Value::as_i64).unwrap_or(0)
        } else {
            0
        }
    }
    fn preserve_model_api(&self, credentials: &Value) -> bool {
        is_oauth(Some(credentials))
    }
    fn supplemental_models(
        &self,
        definition: &Definition,
        credentials: Option<&Value>,
    ) -> Vec<Model> {
        if !is_oauth(credentials) {
            return Vec::new();
        }
        let mut model = Model::new("gpt-image-2", "openai-responses", &definition.id);
        model.name = "GPT Image".into();
        model.info.chat = Some(false);
        model.info.image_output = Some(true);
        model.info.tool_calling = Some(false);
        models::configure_model(definition, credentials, &mut model);
        vec![model]
    }
    async fn prepare_fetch(
        &self,
        _definition: &Definition,
        credentials: &Value,
        target: &mut Url,
        headers: &mut BTreeMap<String, String>,
        body: &mut Option<Vec<u8>>,
        auth: &mut ResolvedProviderAuth,
    ) -> PluginResult<()> {
        media::prepare(is_oauth(Some(credentials)), target, headers, body)?;
        if target.path().starts_with("/backend-api/codex/images/") {
            merge_headers(
                &mut auth.headers,
                [
                    ("originator".into(), "codex_cli_rs".into()),
                    ("accept".into(), "application/json".into()),
                ],
            );
        }
        if is_oauth(Some(credentials)) {
            merge_headers(
                &mut auth.headers,
                codex_version::headers(&codex_version::current().await),
            );
        }
        Ok(())
    }
    fn finalize_fetch(
        &self,
        credentials: &Value,
        target: &Url,
        headers: &mut BTreeMap<String, String>,
        body: &mut Option<Vec<u8>>,
    ) -> PluginResult<()> {
        let _ = headers;
        fast::finalize(is_oauth(Some(credentials)), target, body);
        Ok(())
    }
    async fn usage(
        &self,
        auth: &ResolvedProviderAuth,
        account_id: Option<&str>,
        client: &dyn HttpClient,
    ) -> Result<Value, String> {
        usage::fetch(auth, account_id, client).await
    }

    async fn fetch_override(
        &self,
        definition: &Definition,
        request: &nora_provider_plugin_sdk::FetchRequest,
        client: &dyn HttpClient,
    ) -> PluginResult<Option<nora_provider_plugin_sdk::FetchResponse>> {
        media::fetch(definition, request, client).await
    }
}

pub fn model_urls_with_version(base: &str, version: &str) -> PluginResult<Vec<Url>> {
    let mut url = nora_provider::discovery::validated_url(base)?;
    url.set_path("/backend-api/codex/models");
    url.query_pairs_mut().append_pair("client_version", version);
    Ok(vec![url])
}

fn cli_models(definition: &Definition, credentials: &Value, catalog: &ModelsCatalog) -> Vec<Model> {
    let Some(path) = codex_version::models_cache_path() else {
        return Vec::new();
    };
    let Ok(metadata) = std::fs::metadata(&path) else {
        return Vec::new();
    };
    if metadata.len() > 8 * 1024 * 1024 {
        return Vec::new();
    }
    let Some(payload) = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    else {
        return Vec::new();
    };
    let known = models::catalog_models_with_catalog(definition, Some(credentials), catalog)
        .unwrap_or_default()
        .into_iter()
        .map(|model| (model.id.clone(), model))
        .collect();
    parsing::entries(&payload, Kind::OpenAI)
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| parsing::model(entry, Kind::OpenAI, definition, credentials, &known))
        .collect()
}

fn source_error(error: codex_source::SourceError) -> PluginError {
    PluginError::authentication(error.message())
}

fn source_client(proxy: Option<&NetworkProxy>) -> PluginResult<nora_ai::ReqwestHttpClient> {
    Ok(nora_ai::ReqwestHttpClient::with_client(
        nora_provider_plugin_sdk::http_client(proxy)?,
    ))
}
