use async_trait::async_trait;
use nora_ai::{
    HttpClient, InputModality, Model, ModelsCatalog, OAuthCredentials, OAuthError, OAuthProvider,
    OAuthProviderId, ResolvedProviderAuth,
};
use nora_provider::{adapter::is_oauth, definition::Definition, models, ProviderAdapter};
use nora_provider_auth::{OAuthLogin, OAuthLoginCallbacks, XaiOAuthProvider};
use nora_provider_plugin_sdk::{NetworkProxy, PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use url::Url;

#[path = "grok_source.rs"]
mod grok_source;
#[path = "usage.rs"]
pub mod usage;

#[path = "media.rs"]
mod media;

#[path = "request.rs"]
mod request;

#[path = "namespaces.rs"]
mod namespaces;

#[path = "rate_limits.rs"]
mod rate_limits;

#[derive(Debug, Default)]
pub struct Xai {
    rate_limits: rate_limits::RateLimits,
}

#[cfg(test)]
#[path = "../test/rate_limit.test.rs"]
mod rate_limit_tests;

#[async_trait]
impl ProviderAdapter for Xai {
    fn transform_response(
        &self,
        request: &nora_provider_plugin_sdk::FetchRequest,
        response: nora_provider_plugin_sdk::FetchResponse,
    ) -> PluginResult<nora_provider_plugin_sdk::FetchResponse> {
        namespaces::response(request, response)
    }
    fn initialize_data_directory(&self, directory: &std::path::Path) -> PluginResult<()> {
        self.rate_limits.initialize(directory)
    }
    fn id(&self) -> Option<&'static str> {
        Some("xai")
    }
    async fn login(
        &self,
        callbacks: OAuthLoginCallbacks,
        client: &dyn HttpClient,
    ) -> Result<OAuthCredentials, OAuthError> {
        OAuthLogin::login_xai(callbacks, client).await
    }
    async fn refresh(
        &self,
        credentials: &OAuthCredentials,
        client: &dyn HttpClient,
    ) -> PluginResult<OAuthCredentials> {
        XaiOAuthProvider::default()
            .refresh(credentials, client)
            .await
            .map_err(|_| {
                PluginError::authentication(
                    "The provider account could not be renewed. Sign in again.",
                )
            })
    }
    fn enrich_credentials(&self, value: &Value, credentials: &mut OAuthCredentials) {
        if credentials.provider != OAuthProviderId::XAI {
            return;
        }
        for field in ["userId", "teamId", "organizationId"] {
            let stored = credentials
                .extras
                .get(field)
                .and_then(|value| value.as_str());
            if stored.is_none_or(str::is_empty) {
                if let Some(text) = value.get(field).and_then(Value::as_str).filter(|text| {
                    !text.is_empty() && text.len() <= 512 && !text.chars().any(char::is_control)
                }) {
                    credentials
                        .extras
                        .insert(field.into(), nora_ai::JsonValue::string(text));
                }
            }
        }
        if self.is_source(credentials) && !credentials.extras.contains_key("userId") {
            if let Some(subject) = credentials
                .extras
                .get(nora_provider_auth::grok_source::ACCOUNT_ID_KEY)
                .cloned()
            {
                credentials.extras.insert("userId".into(), subject);
            }
        }
    }
    fn supports_account_discovery(&self) -> bool {
        true
    }
    async fn discover_accounts(
        &self,
        client: &dyn HttpClient,
        proxy: Option<&NetworkProxy>,
    ) -> PluginResult<Vec<OAuthCredentials>> {
        nora_provider_auth::grok_source::discover_current_account_with_network(client, proxy)
            .await
            .map(|account| account.into_iter().collect())
            .map_err(PluginError::transport)
    }
    async fn enrich_account(
        &self,
        credentials: &mut OAuthCredentials,
        client: &dyn HttpClient,
        _proxy: Option<&NetworkProxy>,
    ) -> PluginResult<()> {
        if credentials.provider != OAuthProviderId::XAI {
            return Ok(());
        }
        let identity_exists = credentials
            .extras
            .get("accountIdentity")
            .is_some_and(|value| value.as_value().is_object());
        if identity_exists {
            return Ok(());
        }
        // A userId saved by /user remains an identity after its access token
        // expires. Only older sessions without that verified id need /user.
        let stored_user = credentials
            .extras
            .get("userId")
            .and_then(|value| value.as_str())
            .filter(|value| {
                !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
            });
        let user = if stored_user.is_some() || self.is_source(credentials) {
            None
        } else {
            Some(
                nora_provider_auth::validate_xai_oauth_session(client, &credentials.access_token)
                    .await
                    .map_err(|_| {
                        PluginError::authentication("xAI could not verify this account. Try again.")
                    })?,
            )
        };
        if let Some(user) = user {
            credentials.extras.extend(user.credential_extras());
        }
        let subject = credentials
            .extras
            .get("userId")
            .and_then(|value| value.as_str())
            .filter(|value| {
                !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
            })
            .ok_or_else(|| {
                PluginError::authentication("xAI did not return a stable account identity.")
            })?;
        // A team or organization selects a separate billing account even when
        // the person signing in is the same.
        let scope = ["teamId", "organizationId"].map(|key| {
            credentials
                .extras
                .get(key)
                .and_then(|value| value.as_str())
                .unwrap_or_default()
        });
        let scope = serde_json::to_string(&scope)
            .map_err(|_| PluginError::invalid("The xAI account identity is invalid."))?;
        if scope.len() > 512 || scope.chars().any(char::is_control) {
            return Err(PluginError::authentication(
                "xAI returned an invalid account identity.",
            ));
        }
        let identity = json!({"issuer":"https://auth.x.ai","subject":subject,"scope":scope});
        credentials
            .extras
            .insert("accountIdentity".into(), identity.into());
        Ok(())
    }
    fn is_source(&self, credentials: &OAuthCredentials) -> bool {
        grok_source::is_bound(credentials)
    }
    async fn authorize_source(
        &self,
        method: &str,
        client: &dyn HttpClient,
        proxy: Option<&NetworkProxy>,
    ) -> PluginResult<OAuthCredentials> {
        if method != "grokCLI" {
            return Err(PluginError::unsupported(method));
        }
        grok_source::bind_current_account_with_network(client, proxy)
            .await
            .map_err(PluginError::authentication)
    }
    async fn refresh_source(
        &self,
        credentials: &OAuthCredentials,
        force: bool,
        proxy: Option<&NetworkProxy>,
    ) -> PluginResult<()> {
        let reason = if force {
            grok_source::AgentAuthResolveReason::Refresh
        } else {
            grok_source::AgentAuthResolveReason::Initial
        };
        grok_source::resolve(credentials, reason, proxy).await?;
        Ok(())
    }
    async fn resolve_source(
        &self,
        credentials: &OAuthCredentials,
        proxy: Option<&NetworkProxy>,
    ) -> PluginResult<ResolvedProviderAuth> {
        let mut auth = grok_source::resolve(
            credentials,
            grok_source::AgentAuthResolveReason::Initial,
            proxy,
        )
        .await?;
        rate_limits::bind_auth(credentials, &mut auth);
        Ok(auth)
    }
    fn enrich_auth(&self, credentials: &OAuthCredentials, auth: &mut ResolvedProviderAuth) {
        if credentials.provider != OAuthProviderId::XAI {
            nora_provider_auth::apply_xai_credentials_contract("xai-api-key", credentials, auth);
            return;
        }
        rate_limits::bind_auth(credentials, auth);
        let version = grok_source::client_version(credentials);
        nora_provider_auth::apply_xai_oauth_session_contract_with_client(
            auth,
            &nora_provider_auth::XaiClientContext::new(version, "headless"),
        );
        let arch = match std::env::consts::ARCH {
            "x86" => "386",
            arch => arch,
        };
        nora_provider::adapter::merge_headers(
            &mut auth.headers,
            [
                ("x-grok-client-identifier".into(), "grok-shell".into()),
                (
                    "user-agent".into(),
                    format!("grok-shell/{version} ({}; {arch})", std::env::consts::OS),
                ),
            ],
        );
    }
    fn endpoint(&self, credentials: Option<&Value>) -> Option<String> {
        is_oauth(credentials).then(|| nora_provider_auth::XAI_OAUTH_PROXY_BASE_URL.into())
    }
    fn retain_model(&self, credentials: Option<&Value>, model: &Model) -> bool {
        is_oauth(credentials)
            || (!model.id.starts_with("grok-build") && !model.id.starts_with("grok-composer"))
    }
    fn fallback_models(
        &self,
        definition: &Definition,
        credentials: Option<&Value>,
        _catalog: &ModelsCatalog,
    ) -> Option<Vec<Model>> {
        if !is_oauth(credentials) {
            return None;
        }
        let mut model = Model::new("grok-4.7", "openai-responses", &definition.id);
        model.name = "Grok 4.7".into();
        model.reasoning = true;
        model.input = vec![InputModality::Text, InputModality::Image];
        model.info.reasoning_efforts =
            vec!["low".into(), "medium".into(), "high".into(), "xhigh".into()];
        models::configure_model(definition, credentials, &mut model);
        Some(vec![model])
    }
    fn bearer_model_list(&self, auth: &ResolvedProviderAuth) -> bool {
        auth.credential_provider_id.as_deref() == Some("xai")
    }
    fn supplemental_models(
        &self,
        definition: &Definition,
        credentials: Option<&Value>,
    ) -> Vec<Model> {
        if !is_oauth(credentials) {
            return Vec::new();
        }
        [
            ("grok-imagine-image", "Grok Imagine", true),
            ("grok-imagine-image-quality", "Grok Imagine Quality", true),
            ("grok-imagine-video", "Grok Imagine Video", false),
            ("grok-imagine-video-1.5", "Grok Imagine Video 1.5", false),
        ]
        .into_iter()
        .map(|(id, name, image)| {
            let mut model = Model::new(id, "openai-responses", &definition.id);
            model.name = name.into();
            model.info.chat = Some(false);
            model.info.tool_calling = Some(false);
            if image {
                model.info.image_output = Some(true);
            } else {
                model.info.video_output = Some(true);
            }
            models::configure_model(definition, credentials, &mut model);
            model
        })
        .collect()
    }
    fn model_visible(&self, entry: &Value, _id: &str, credentials: &Value) -> bool {
        !is_oauth(Some(credentials))
            || _id.starts_with("grok-imagine-")
            || !entry
                .get("api_backend")
                .and_then(Value::as_str)
                .is_some_and(|backend| backend != "responses")
    }
    fn enrich_model(&self, _entry: &Value, model: &mut Model, credentials: &Value) {
        if !is_oauth(Some(credentials)) {
            return;
        }
        model.info.reasoning_efforts.reverse();
        model.input = vec![InputModality::Text, InputModality::Image];
    }
    fn expand_model(&self, model: Model, entry: &Value, credentials: &Value) -> Vec<Model> {
        if is_oauth(Some(credentials)) {
            grok_windows(model, entry)
        } else {
            vec![model]
        }
    }
    fn preserve_model_api(&self, credentials: &Value) -> bool {
        is_oauth(Some(credentials))
    }
    async fn prepare_fetch(
        &self,
        definition: &Definition,
        credentials: &Value,
        target: &mut Url,
        headers: &mut BTreeMap<String, String>,
        body: &mut Option<Vec<u8>>,
        _auth: &mut ResolvedProviderAuth,
    ) -> PluginResult<()> {
        if media::prepare(target, headers, body)? {
            return Ok(());
        }
        if !is_oauth(Some(credentials)) {
            return Ok(());
        }
        if let Some(bytes) = body.as_ref() {
            let mut payload: Value = serde_json::from_slice(bytes)
                .map_err(|_| PluginError::invalid("The Grok request body is invalid."))?;
            let mut changed = payload
                .as_object_mut()
                .is_some_and(|object| object.remove("metadata").is_some());
            let x_search = target.path().ends_with("/responses")
                && definition
                    .config
                    .extra
                    .get("x_search")
                    .and_then(Value::as_bool)
                    == Some(true);
            changed |= request::rewrite(&mut payload, x_search);
            if let Some(model) = payload
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string)
            {
                let (model, window) = grok_model_selection(&model);
                headers.insert("x-grok-model-override".into(), model.into());
                if let Some(window) = window {
                    payload["model"] = json!(model);
                    headers.insert("x-grok-context-window".into(), window.to_string());
                    changed = true;
                }
            }
            if let Some(key) = payload
                .get("prompt_cache_key")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            {
                headers.insert("x-grok-conv-id".into(), key.into());
            }
            if changed {
                *body = Some(
                    serde_json::to_vec(&payload)
                        .map_err(|_| PluginError::invalid("Cannot encode the Grok request."))?,
                );
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
        let mut usage = usage::fetch(auth, account_id, client).await?;
        self.rate_limits
            .apply(auth, &mut usage, chrono::Utc::now().timestamp_millis());
        Ok(usage)
    }

    fn observe_response(
        &self,
        credentials: &Value,
        status: u16,
        headers: &BTreeMap<String, String>,
    ) {
        self.rate_limits.observe(
            credentials,
            status,
            headers,
            chrono::Utc::now().timestamp_millis(),
        );
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
pub fn grok_model_selection(id: &str) -> (&str, Option<u64>) {
    let Some((model, window)) = id.rsplit_once('@').filter(|(model, _)| !model.is_empty()) else {
        return (id, None);
    };
    let (digits, multiplier) = window
        .strip_suffix('k')
        .map(|digits| (digits, 1000))
        .unwrap_or((window, 1));
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return (id, None);
    }
    let window = digits
        .parse::<u64>()
        .ok()
        .and_then(|value| value.checked_mul(multiplier))
        .filter(|value| *value > 0);
    if window.is_some() {
        (model, window)
    } else {
        (id, None)
    }
}

pub fn grok_windows(mut model: Model, entry: &Value) -> Vec<Model> {
    let fallback = entry
        .get("context_window")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut windows = Vec::new();
    if let Some(values) = entry.get("context_windows").and_then(Value::as_array) {
        for value in values {
            if let Some(window) = value.as_u64().filter(|value| *value > 0) {
                if !windows.contains(&window) {
                    windows.push(window);
                }
            }
        }
    }
    if windows.is_empty() && fallback > 0 {
        windows.push(fallback);
    }
    let base = if windows.contains(&fallback) {
        fallback
    } else {
        windows.first().copied().unwrap_or(fallback)
    };
    if base > 0 {
        model.context_window = base;
    }
    let mut models = vec![model.clone()];
    for window in windows.into_iter().filter(|window| *window != base) {
        let label = if window % 1000 == 0 {
            format!("{}k", window / 1000)
        } else {
            window.to_string()
        };
        let mut variant = model.clone();
        variant.id = format!("{}@{label}", model.id);
        variant.name = format!("{} @ {label}", model.name);
        variant.context_window = window;
        models.push(variant);
    }
    models
}

#[cfg(test)]
#[path = "../test/account_identity.test.rs"]
mod account_identity_tests;

#[cfg(test)]
#[path = "../test/request.test.rs"]
mod request_tests;
