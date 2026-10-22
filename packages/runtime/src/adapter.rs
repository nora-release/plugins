use crate::definition::Definition;
use async_trait::async_trait;
use nora_ai::{
    HttpClient, Model, ModelsCatalog, OAuthCredentials, OAuthError, ResolvedProviderAuth,
};
use nora_provider_auth::OAuthLoginCallbacks;
use nora_provider_plugin_sdk::{
    FetchRequest, FetchResponse, NetworkProxy, PluginError, PluginResult,
};
use serde_json::Value;
use std::{collections::BTreeMap, fmt::Debug, sync::Arc};
use url::Url;

/// A native provider owns account and upstream-specific behavior. The runtime
/// only coordinates the plugin protocol, safe HTTP transport, and model caching.
#[async_trait]
pub trait ProviderAdapter: Debug + Send + Sync + 'static {
    fn id(&self) -> Option<&'static str> {
        None
    }
    async fn login(
        &self,
        _callbacks: OAuthLoginCallbacks,
        _client: &dyn HttpClient,
    ) -> Result<OAuthCredentials, OAuthError> {
        Err(OAuthError::UnknownProvider("oauth".into()))
    }
    async fn refresh(
        &self,
        _credentials: &OAuthCredentials,
        _client: &dyn HttpClient,
    ) -> PluginResult<OAuthCredentials> {
        Err(PluginError::unsupported("refresh"))
    }
    fn enrich_credentials(&self, _value: &Value, _credentials: &mut OAuthCredentials) {}
    fn configure_api_credentials(
        &self,
        _inputs: &Value,
        _credentials: &mut OAuthCredentials,
    ) -> PluginResult<()> {
        Ok(())
    }
    fn is_source(&self, _credentials: &OAuthCredentials) -> bool {
        false
    }
    async fn authorize_source(
        &self,
        method: &str,
        _client: &dyn HttpClient,
        _proxy: Option<&NetworkProxy>,
    ) -> PluginResult<OAuthCredentials> {
        Err(PluginError::unsupported(method))
    }
    async fn refresh_source(
        &self,
        _credentials: &OAuthCredentials,
        _force: bool,
        _proxy: Option<&NetworkProxy>,
    ) -> PluginResult<()> {
        Err(PluginError::unsupported("refresh"))
    }
    async fn resolve_source(
        &self,
        _credentials: &OAuthCredentials,
        _proxy: Option<&NetworkProxy>,
    ) -> PluginResult<ResolvedProviderAuth> {
        Err(PluginError::unsupported("source"))
    }
    fn enrich_auth(&self, _credentials: &OAuthCredentials, _auth: &mut ResolvedProviderAuth) {}
    fn endpoint(&self, _credentials: Option<&Value>) -> Option<String> {
        None
    }
    fn catalog<'a>(&self, _credentials: Option<&Value>, default: &'a str) -> &'a str {
        default
    }
    fn model_api(&self, _credentials: Option<&Value>, _native_api: &str) -> Option<String> {
        None
    }
    fn retain_model(&self, _credentials: Option<&Value>, _model: &Model) -> bool {
        true
    }
    fn remote_models(&self, _credentials: Option<&Value>) -> bool {
        true
    }
    fn fallback_models(
        &self,
        _definition: &Definition,
        _credentials: Option<&Value>,
        _catalog: &ModelsCatalog,
    ) -> Option<Vec<Model>> {
        None
    }
    fn supplemental_models(
        &self,
        _definition: &Definition,
        _credentials: Option<&Value>,
    ) -> Vec<Model> {
        Vec::new()
    }
    async fn model_urls(
        &self,
        _base: &str,
        _credentials: &Value,
    ) -> PluginResult<Option<Vec<Url>>> {
        Ok(None)
    }
    fn model_list_headers(
        &self,
        _url: &Url,
        _auth: &ResolvedProviderAuth,
        _headers: &mut BTreeMap<String, String>,
    ) {
    }
    fn bearer_model_list(&self, _auth: &ResolvedProviderAuth) -> bool {
        false
    }
    fn model_id<'a>(&self, _entry: &'a Value, _credentials: &Value) -> Option<&'a str> {
        None
    }
    fn model_visible(&self, _entry: &Value, _id: &str, _credentials: &Value) -> bool {
        true
    }
    fn model_priority(&self, _entry: &Value, _credentials: &Value) -> i64 {
        0
    }
    fn enrich_model(&self, _entry: &Value, _model: &mut Model, _credentials: &Value) {}
    fn expand_model(&self, model: Model, _entry: &Value, _credentials: &Value) -> Vec<Model> {
        vec![model]
    }
    fn preserve_model_api(&self, _credentials: &Value) -> bool {
        false
    }
    async fn fetch_override(
        &self,
        _definition: &Definition,
        _request: &FetchRequest,
        _client: &dyn HttpClient,
    ) -> PluginResult<Option<FetchResponse>> {
        Ok(None)
    }
    async fn prepare_fetch(
        &self,
        _credentials: &Value,
        _target: &mut Url,
        _headers: &mut BTreeMap<String, String>,
        _body: &mut Option<Vec<u8>>,
        _auth: &mut ResolvedProviderAuth,
    ) -> PluginResult<()> {
        Ok(())
    }
    async fn usage(
        &self,
        _auth: &ResolvedProviderAuth,
        _account_id: Option<&str>,
        _client: &dyn HttpClient,
    ) -> Result<Value, String> {
        Ok(crate::usage::unavailable("This provider does not expose a supported subscription usage endpoint for this account."))
    }
}

#[derive(Debug, Default)]
pub struct GenericProvider;
impl ProviderAdapter for GenericProvider {}

pub(crate) fn generic() -> Arc<dyn ProviderAdapter> {
    Arc::new(GenericProvider)
}

pub fn is_oauth(credentials: Option<&Value>) -> bool {
    credentials.is_some_and(|value| !crate::auth::is_api(value))
}

pub fn merge_headers(
    headers: &mut BTreeMap<String, String>,
    additions: impl IntoIterator<Item = (String, String)>,
) {
    for (name, value) in additions {
        headers.retain(|key, _| !key.eq_ignore_ascii_case(&name));
        headers.insert(name, value);
    }
}
