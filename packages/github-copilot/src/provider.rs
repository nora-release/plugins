use async_trait::async_trait;
use nora_ai::{
    HttpClient, JsonValue, Model, OAuthCredentials, OAuthError, OAuthProvider, ResolvedProviderAuth,
};
use nora_provider::{adapter::is_oauth, ProviderAdapter};
use nora_provider_auth::{GitHubCopilotOAuthProvider, OAuthLogin, OAuthLoginCallbacks};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
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
        GitHubCopilotOAuthProvider::default()
            .refresh(credentials, client)
            .await
            .map_err(|_| {
                PluginError::authentication(
                    "The provider account could not be renewed. Sign in again.",
                )
            })
    }
    fn enrich_auth(&self, credentials: &OAuthCredentials, auth: &mut ResolvedProviderAuth) {
        auth.headers
            .extend(nora_ai::copilot_editor_headers("vscode-chat"));
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
        _auth: &ResolvedProviderAuth,
        headers: &mut BTreeMap<String, String>,
    ) {
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
