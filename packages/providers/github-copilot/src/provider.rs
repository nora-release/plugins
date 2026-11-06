use async_trait::async_trait;
use nora_ai::{
    HttpClient, JsonValue, Model, OAuthCredentials, OAuthError, OAuthProvider, OAuthProviderId,
    ResolvedProviderAuth,
};
use nora_provider::{adapter::is_oauth, ProviderAdapter};
use nora_provider_auth::{
    copilot_client, copilot_editors, copilot_identity, GitHubCopilotOAuthProvider, OAuthLogin,
    OAuthLoginCallbacks,
};
use nora_provider_plugin_sdk::model_resolution::{ModelResolution, ResolveModelRequest};
use nora_provider_plugin_sdk::{NetworkProxy, PluginError, PluginResult};
use serde_json::Value;
use std::collections::BTreeMap;
use url::Url;

#[path = "auto.rs"]
mod auto;
#[path = "refusals.rs"]
mod refusals;
#[path = "terms.rs"]
mod terms;
#[path = "usage.rs"]
pub mod usage;

#[derive(Debug)]
pub struct Copilot {
    auto: auto::Resolver,
    terms: terms::Approvals,
    refused: std::sync::Arc<refusals::Refusals>,
}

impl Default for Copilot {
    fn default() -> Self {
        let refused = std::sync::Arc::new(refusals::Refusals::default());
        Self {
            auto: auto::Resolver::new(refused.clone()),
            terms: terms::Approvals::default(),
            refused,
        }
    }
}

#[async_trait]
impl ProviderAdapter for Copilot {
    fn id(&self) -> Option<&'static str> {
        Some("github-copilot")
    }
    async fn login(
        &self,
        callbacks: OAuthLoginCallbacks,
        client: &dyn HttpClient,
    ) -> Result<OAuthCredentials, OAuthError> {
        OAuthLogin::login_github_copilot(callbacks, client).await
    }
    async fn login_with_inputs(
        &self,
        callbacks: OAuthLoginCallbacks,
        inputs: Value,
        client: &dyn HttpClient,
    ) -> Result<OAuthCredentials, OAuthError> {
        let host = match inputs.get("host") {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(host)) => {
                copilot_identity::normalize_host(host).ok_or_else(|| {
                    OAuthError::InvalidResponse(
                        "Copilot signs in on github.com or an enterprise's <name>.ghe.com.".into(),
                    )
                })?
            }
            _ => {
                return Err(OAuthError::InvalidResponse(
                    "The GitHub account host is invalid.".into(),
                ))
            }
        };
        let mut config = nora_provider_auth::GitHubCopilotLoginConfig::default();
        if !host.is_empty() {
            config.device_code_url = format!("https://{host}/login/device/code");
            config.access_token_url = format!("https://{host}/login/oauth/access_token");
            config.copilot_token_url =
                copilot_identity::api_url(&host, "/copilot_internal/v2/token");
            config.github_user_url = copilot_identity::api_url(&host, "/user");
            config.github_host = host;
        }
        OAuthLogin::login_github_copilot_with_config(config, callbacks, client).await
    }
    async fn refresh(
        &self,
        credentials: &OAuthCredentials,
        client: &dyn HttpClient,
    ) -> PluginResult<OAuthCredentials> {
        let host = copilot_identity::host(credentials)
            .map_err(|_| PluginError::authentication("The GitHub account host is invalid."))?;
        let provider = GitHubCopilotOAuthProvider::new(
            copilot_identity::api_url(&host, "/copilot_internal/v2/token"),
            GitHubCopilotOAuthProvider::default().extra_headers,
        );
        let mut renewed = provider
            .refresh(credentials, client)
            .await
            .map_err(oauth_error)?;
        self.enrich_account(&mut renewed, client, None).await?;
        Ok(renewed)
    }
    async fn enrich_account(
        &self,
        credentials: &mut OAuthCredentials,
        client: &dyn HttpClient,
        _proxy: Option<&NetworkProxy>,
    ) -> PluginResult<()> {
        if credentials.provider != OAuthProviderId::GitHubCopilot
            || copilot_editors::is_bound(credentials)
            || copilot_identity::has_identity(credentials)
        {
            return Ok(());
        }
        copilot_identity::enrich(credentials, client)
            .await
            .map_err(oauth_error)
    }
    fn supports_account_discovery(&self) -> bool {
        true
    }
    fn supports_model_resolution(&self) -> bool {
        true
    }
    async fn resolve_model(
        &self,
        definition: &nora_provider::definition::Definition,
        request: ResolveModelRequest,
        auth: &ResolvedProviderAuth,
        client: &dyn HttpClient,
    ) -> PluginResult<Option<ModelResolution>> {
        self.auto.resolve(definition, request, auth, client).await
    }
    fn supplemental_models(
        &self,
        definition: &nora_provider::definition::Definition,
        credentials: Option<&Value>,
    ) -> Vec<Model> {
        if !is_oauth(credentials) {
            return Vec::new();
        }
        let mut model = Model::new("auto", "github-copilot-chat", &definition.id);
        model.name = "Auto".into();
        model.input = vec![nora_ai::InputModality::Text, nora_ai::InputModality::Image];
        model.context_window = 0;
        model.max_tokens = 0;
        model.info.chat = Some(true);
        model.info.tool_calling = Some(true);
        nora_provider::models::configure_model(definition, credentials, &mut model);
        vec![model]
    }
    fn fallback_models(
        &self,
        _definition: &nora_provider::definition::Definition,
        credentials: Option<&Value>,
        _catalog: &nora_ai::ModelsCatalog,
    ) -> Option<Vec<Model>> {
        is_oauth(credentials).then(Vec::new)
    }
    async fn discover_accounts(
        &self,
        client: &dyn HttpClient,
        _proxy: Option<&NetworkProxy>,
    ) -> PluginResult<Vec<OAuthCredentials>> {
        let home = local_home()?;
        copilot_editors::discover(&home, &std::env::vars().collect(), client)
            .await
            .map_err(source_error)
    }
    fn is_source(&self, credentials: &OAuthCredentials) -> bool {
        copilot_editors::is_bound(credentials)
    }
    async fn authorize_source(
        &self,
        method: &str,
        client: &dyn HttpClient,
        _proxy: Option<&NetworkProxy>,
    ) -> PluginResult<OAuthCredentials> {
        if method != copilot_editors::KIND {
            return Err(PluginError::unsupported(method));
        }
        self.discover_accounts(client, None)
            .await
            .and_then(|accounts| {
                accounts
                    .into_iter()
                    .next()
                    .ok_or_else(|| source_error(copilot_editors::EditorsError::NotSignedIn))
            })
    }
    async fn refresh_source(
        &self,
        credentials: &OAuthCredentials,
        force: bool,
        proxy: Option<&NetworkProxy>,
    ) -> PluginResult<()> {
        copilot_editors::resolve_with_force(credentials, force, &source_client(proxy)?)
            .await
            .map_err(source_error)?;
        Ok(())
    }
    async fn resolve_source(
        &self,
        credentials: &OAuthCredentials,
        proxy: Option<&NetworkProxy>,
    ) -> PluginResult<ResolvedProviderAuth> {
        let current = copilot_editors::resolve(credentials, &source_client(proxy)?)
            .await
            .map_err(source_error)?;
        let mut auth = nora_provider::auth::base_auth("github-copilot", &current);
        self.enrich_auth(&current, &mut auth);
        Ok(auth)
    }
    fn enrich_auth(&self, credentials: &OAuthCredentials, auth: &mut ResolvedProviderAuth) {
        if auth.base_url.is_none() {
            if let Ok(host) = copilot_identity::host(credentials) {
                if !host.is_empty() {
                    auth.base_url = Some(format!("https://copilot-api.{host}"));
                }
            }
        }
        if credentials
            .extras
            .get("noraCopilotCLI")
            .is_some_and(|value| value.as_value() == &Value::Bool(true))
        {
            nora_provider::adapter::merge_headers(&mut auth.headers, copilot_client::cli_headers());
        } else {
            nora_provider::adapter::merge_headers(&mut auth.headers, copilot_client::api_headers());
        }
        if let Some(token) = credentials.refresh_token.as_deref() {
            auth.metadata
                .insert("githubOAuthToken".into(), JsonValue::string(token));
        }
        if let Some(host) = credentials.extras.get("githubHost") {
            auth.metadata.insert("githubHost".into(), host.clone());
        }
    }
    fn endpoint(&self, credentials: Option<&Value>) -> Option<String> {
        if !is_oauth(credentials) {
            return None;
        }
        credentials
            .and_then(|value| value.pointer("/extras/endpoint"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                credentials
                    .and_then(|value| value.pointer("/extras/githubHost"))
                    .and_then(Value::as_str)
                    .and_then(copilot_identity::normalize_host)
                    .filter(|host| !host.is_empty())
                    .map(|host| format!("https://copilot-api.{host}"))
            })
    }
    fn live_endpoint(&self, credentials: &Value, auth: &ResolvedProviderAuth) -> Option<String> {
        is_oauth(Some(credentials))
            .then(|| auth.base_url.clone())
            .flatten()
    }
    fn model_api(&self, credentials: Option<&Value>, native_api: &str) -> Option<String> {
        if !is_oauth(credentials) {
            return None;
        }
        Some(
            match native_api {
                "anthropic-messages" | "github-copilot-anthropic-messages" => {
                    "github-copilot-anthropic-messages"
                }
                "openai-responses" | "github-copilot-openai-responses" => {
                    "github-copilot-openai-responses"
                }
                _ => "github-copilot-chat",
            }
            .into(),
        )
    }
    fn selectable_model_apis(&self, credentials: Option<&Value>) -> &'static [&'static str] {
        if is_oauth(credentials) {
            &[
                "openai-completions",
                "openai-responses",
                "anthropic-messages",
            ]
        } else {
            &[]
        }
    }
    fn model_list_headers(
        &self,
        _url: &Url,
        auth: &ResolvedProviderAuth,
        headers: &mut BTreeMap<String, String>,
    ) {
        if auth
            .headers
            .get("copilot-integration-id")
            .map(String::as_str)
            == Some("copilot-developer-cli")
        {
            return;
        }
        nora_provider::adapter::merge_headers(headers, copilot_client::api_headers());
    }
    fn bearer_model_list(&self, _auth: &ResolvedProviderAuth) -> bool {
        true
    }
    fn model_visible(&self, entry: &Value, id: &str, credentials: &Value) -> bool {
        visible(entry, id) && !self.refused.account(credentials).contains(id)
    }
    fn observed_error_statuses(&self, credentials: &Value) -> &'static [u16] {
        if is_oauth(Some(credentials)) {
            &[400]
        } else {
            &[]
        }
    }
    fn observe_response_error(&self, credentials: &Value, model: &str, status: u16, body: &[u8]) {
        if is_oauth(Some(credentials)) && status == 400 && refusals::model_refused(body) {
            self.refused.account(credentials).remember(model);
        }
    }
    fn enrich_model(&self, entry: &Value, model: &mut Model, _credentials: &Value) {
        let endpoints = entry
            .get("supported_endpoints")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if let Some(api) = model_api(endpoints) {
            model.api = api.into();
        }
    }
    fn preserve_model_api(&self, _credentials: &Value) -> bool {
        true
    }
    async fn prepare_fetch(
        &self,
        _definition: &nora_provider::definition::Definition,
        _credentials: &Value,
        target: &mut Url,
        _headers: &mut BTreeMap<String, String>,
        _body: &mut Option<Vec<u8>>,
        _auth: &mut ResolvedProviderAuth,
    ) -> PluginResult<()> {
        let path = target.path().to_string();
        for suffix in ["/v1/chat/completions", "/v1/responses"] {
            if let Some(prefix) = path.strip_suffix(suffix) {
                target.set_path(&format!("{prefix}{}", suffix.trim_start_matches("/v1")));
            }
        }
        Ok(())
    }
    async fn before_send(
        &self,
        _definition: &nora_provider::definition::Definition,
        credentials: &Value,
        request: &nora_ai::HttpRequest,
        client: &dyn HttpClient,
    ) -> PluginResult<()> {
        if is_oauth(Some(credentials)) {
            self.terms.before_send(request, client).await;
        }
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
}

fn local_home() -> PluginResult<std::path::PathBuf> {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .filter(|home| home.is_absolute())
        .ok_or_else(|| PluginError::authentication("The home directory is unavailable."))
}

fn source_error(error: copilot_editors::EditorsError) -> PluginError {
    match error {
        copilot_editors::EditorsError::Unavailable | copilot_editors::EditorsError::InvalidFile => {
            PluginError::transport(error.message())
        }
        _ => PluginError::authentication(error.message()),
    }
}

fn oauth_error(error: OAuthError) -> PluginError {
    if copilot_identity::authentication_rejected(&error) {
        PluginError::authentication("GitHub refused this Copilot sign-in. Sign in again.")
    } else {
        PluginError::transport("GitHub Copilot could not verify this account right now. Credentials were kept; try again.")
    }
}

fn source_client(proxy: Option<&NetworkProxy>) -> PluginResult<nora_ai::ReqwestHttpClient> {
    Ok(
        nora_ai::ReqwestHttpClient::with_client(nora_provider_plugin_sdk::http_client(proxy)?)
            .with_observer_fn(|| async {
                nora_provider_plugin_sdk::before_http_request()
                    .await
                    .map_err(|error| nora_ai::HttpClientError::Transport(error.to_string()))
            }),
    )
}

fn visible(entry: &Value, id: &str) -> bool {
    if entry.pointer("/capabilities/type").and_then(Value::as_str) != Some("chat")
        || entry.get("vendor").and_then(Value::as_str) == Some("Experimental")
        || ["copilot-search", "exec-agent", "trajectory"]
            .iter()
            .any(|prefix| id.starts_with(prefix))
        || ["-secondary", "-tertiary", "-4th", "-free-auto"]
            .iter()
            .any(|suffix| id.ends_with(suffix))
    {
        return false;
    }
    if entry.get("model_picker_enabled").and_then(Value::as_bool) != Some(true)
        && entry
            .get("model_picker_category")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
    {
        return false;
    }
    if entry.pointer("/policy/state").and_then(Value::as_str) != Some("enabled")
        && entry
            .pointer("/policy/terms")
            .and_then(Value::as_str)
            .is_none_or(|terms| terms.is_empty())
        && !(entry.get("policy").is_none()
            && entry.get("model_picker_enabled").and_then(Value::as_bool) == Some(true))
    {
        return false;
    }
    if let Some(endpoints) = entry
        .get("supported_endpoints")
        .and_then(Value::as_array)
        .filter(|values| !values.is_empty())
    {
        if model_api(endpoints).is_none() {
            return false;
        }
    }
    true
}

fn model_api(endpoints: &[Value]) -> Option<&'static str> {
    if endpoints.is_empty() {
        return Some("github-copilot-chat");
    }
    let normalized = endpoints
        .iter()
        .filter_map(Value::as_str)
        .map(|endpoint| {
            let endpoint = endpoint.trim_end_matches('/');
            endpoint.strip_prefix("/v1").unwrap_or(endpoint)
        })
        .collect::<Vec<_>>();
    for (endpoint, api) in [
        ("/messages", "github-copilot-anthropic-messages"),
        ("/responses", "github-copilot-openai-responses"),
        ("/chat/completions", "github-copilot-chat"),
    ] {
        if normalized.contains(&endpoint) {
            return Some(api);
        }
    }
    None
}

#[cfg(test)]
#[path = "../test/login.test.rs"]
mod login_tests;

#[cfg(test)]
#[path = "../test/renewal.test.rs"]
mod renewal_tests;

#[cfg(test)]
#[path = "../test/model_api.test.rs"]
mod model_api_tests;
