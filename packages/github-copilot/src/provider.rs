use async_trait::async_trait;
use nora_ai::{
    HttpClient, JsonValue, Model, OAuthCredentials, OAuthError, OAuthProvider, OAuthProviderId,
    ResolvedProviderAuth,
};
use nora_provider::{adapter::is_oauth, ProviderAdapter};
use nora_provider_auth::{
    copilot_editors, copilot_identity, GitHubCopilotOAuthProvider, OAuthLogin, OAuthLoginCallbacks,
};
use nora_provider_plugin_sdk::{NetworkProxy, PluginError, PluginResult};
use serde_json::Value;
use std::collections::BTreeMap;
use url::Url;

#[path = "usage.rs"]
pub mod usage;

#[derive(Debug)]
pub struct Copilot;

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
        let mut renewed = provider.refresh(credentials, client).await.map_err(|_| {
            PluginError::authentication("The provider account could not be renewed. Sign in again.")
        })?;
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
            .map_err(|_| {
                PluginError::authentication("GitHub could not verify the account. Sign in again.")
            })
    }
    fn supports_account_discovery(&self) -> bool {
        true
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
        if credentials
            .extras
            .get("noraCopilotCLI")
            .is_some_and(|value| value.as_value() == &Value::Bool(true))
        {
            auth.headers.extend(BTreeMap::from([
                ("editor-version".into(), "copilot/1.0.88".into()),
                (
                    "copilot-integration-id".into(),
                    "copilot-developer-cli".into(),
                ),
                ("x-github-api-version".into(), "2026-07-01".into()),
                ("user-agent".into(), "copilot/1.0.88".into()),
            ]));
        } else {
            auth.headers
                .extend(nora_ai::copilot_editor_headers("vscode-chat"));
        }
        if let Some(token) = credentials.refresh_token.as_deref() {
            auth.metadata
                .insert("githubOAuthToken".into(), JsonValue::string(token));
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
        nora_provider::adapter::merge_headers(
            headers,
            nora_ai::copilot_editor_headers("vscode-chat"),
        );
    }
    fn bearer_model_list(&self, _auth: &ResolvedProviderAuth) -> bool {
        true
    }
    fn model_visible(&self, entry: &Value, id: &str, _credentials: &Value) -> bool {
        visible(entry, id)
    }
    fn enrich_model(&self, entry: &Value, model: &mut Model, _credentials: &Value) {
        if let Some(endpoints) = entry.get("supported_endpoints").and_then(Value::as_array) {
            model.api = if endpoints
                .iter()
                .any(|value| value.as_str() == Some("/v1/messages"))
            {
                "github-copilot-anthropic-messages"
            } else if endpoints
                .iter()
                .any(|value| matches!(value.as_str(), Some("/responses" | "ws:/responses")))
            {
                "github-copilot-openai-responses"
            } else {
                "github-copilot-chat"
            }
            .into();
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
    PluginError::authentication(error.message())
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
        if !endpoints.iter().any(|value| {
            matches!(
                value.as_str(),
                Some("/v1/messages" | "/chat/completions" | "/responses" | "ws:/responses")
            )
        }) {
            return false;
        }
    }
    true
}
