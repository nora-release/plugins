use crate::{provider::usage::resets, provider::OpenAI};
use async_trait::async_trait;
use nora_ai::{OAuthProviderId, ReqwestHttpClient};
use nora_provider::{auth, definition::Definition, BundledProvider};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, PluginError,
    PluginResult, ProviderPlugin,
};
use serde_json::Value;
use std::sync::{Arc, RwLock};

#[path = "warmup.rs"]
mod warmup;
pub(crate) struct OpenAIPlugin {
    warmup_catalog: RwLock<Option<std::path::PathBuf>>,
    inner: BundledProvider,
    config: RwLock<Option<(Definition, Option<NetworkProxy>)>>,
}

impl Default for OpenAIPlugin {
    fn default() -> Self {
        Self {
            inner: BundledProvider::new(OpenAI),
            warmup_catalog: RwLock::new(None),
            config: RwLock::new(None),
        }
    }
}

impl OpenAIPlugin {
    async fn reset_context(
        &self,
        params: &Value,
    ) -> PluginResult<(nora_ai::ResolvedProviderAuth, ReqwestHttpClient)> {
        self.reset_context_for(params, true).await
    }
    async fn reset_context_for(
        &self,
        params: &Value,
        confirmation: bool,
    ) -> PluginResult<(nora_ai::ResolvedProviderAuth, ReqwestHttpClient)> {
        if confirmation && params["confirmed"] != true {
            return Err(PluginError::invalid(
                "Confirm the action for a saved account before using a reset credit.",
            ));
        }
        self.account_context(params).await
    }
    async fn account_context(
        &self,
        params: &Value,
    ) -> PluginResult<(nora_ai::ResolvedProviderAuth, ReqwestHttpClient)> {
        if params["account"].as_str().is_none_or(|account| {
            account.is_empty()
                || account.len() > 128
                || account.trim() != account
                || account.chars().any(char::is_control)
        }) {
            return Err(PluginError::invalid("Choose a saved ChatGPT account."));
        }
        let (definition, default_proxy) = self
            .config
            .read()
            .map_err(|_| PluginError::invalid("The provider configuration is unavailable."))?
            .clone()
            .ok_or_else(|| PluginError::invalid("Initialize OpenAI before using it."))?;
        let value = params
            .get("credentials")
            .ok_or_else(|| PluginError::authentication("Connect a ChatGPT account."))?;
        if auth::is_api(value) {
            return Err(PluginError::unsupported(
                "ChatGPT account operations for an API key",
            ));
        }
        let credentials = auth::decode(&definition, Some(value))?;
        if credentials.provider != OAuthProviderId::OpenAICodex {
            return Err(PluginError::authentication("Connect a ChatGPT account."));
        }
        let proxy: Option<NetworkProxy> =
            serde_json::from_value(params.get("networkProxy").cloned().unwrap_or(Value::Null))
                .map_err(|_| PluginError::invalid("The network proxy configuration is invalid."))?;
        let (_, resolved) = auth::resolve_with_network(
            &definition,
            value,
            proxy.as_ref().or(default_proxy.as_ref()),
        )
        .await?;
        let client =
            nora_provider::transport::http1_client(proxy.as_ref().or(default_proxy.as_ref()))?;
        let client = ReqwestHttpClient::with_client(client).with_observer_fn(|| async {
            nora_provider_plugin_sdk::before_http_request()
                .await
                .map_err(|error| nora_ai::HttpClientError::Transport(error.to_string()))
        });
        Ok((resolved, client))
    }
}

#[async_trait]
impl ProviderPlugin for OpenAIPlugin {
    fn supports_client_observation(&self) -> bool {
        true
    }
    async fn observe_client(
        &self,
        observation: nora_provider_plugin_sdk::ClientObservation,
    ) -> PluginResult<()> {
        observation.validate()?;
        crate::provider::codex_version::observe(
            observation.user_agent.as_deref(),
            observation.version.as_deref(),
            observation.originator.as_deref(),
        );
        Ok(())
    }
    fn supports_alpha_search(&self) -> bool {
        true
    }
    async fn alpha_search(
        &self,
        context: CallContext,
        request: Value,
        headers: std::collections::BTreeMap<String, String>,
    ) -> PluginResult<Value> {
        if !context.subscription_accounts_enabled {
            return Err(PluginError::unsupported(
                "subscription account search while subscriptions are disabled",
            ));
        }
        let (mut auth, client) = self.account_context(&serde_json::json!({"account":context.account,"credentials":context.credentials,"networkProxy":context.network_proxy})).await?;
        crate::provider::codex_version::observe_headers(&headers);
        auth.headers.extend(crate::provider::codex_version::headers(
            &crate::provider::codex_version::current().await,
        ));
        crate::alpha_search::search(&auth, request, headers, &client).await
    }
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        *self
            .warmup_catalog
            .write()
            .map_err(|_| PluginError::invalid("The provider configuration is unavailable."))? =
            Some(params.data_directory.join("models.json"));
        let mut definition = Definition::parse(params.manifest.clone())?;
        definition.adapter = Arc::new(OpenAI);
        let proxy: Option<NetworkProxy> = serde_json::from_value(
            params
                .manifest
                .pointer("/contributes/provider/config/networkProxy")
                .cloned()
                .unwrap_or(Value::Null),
        )
        .map_err(|_| PluginError::invalid("The network proxy configuration is invalid."))?;
        let mut result = self.inner.initialize(params).await?;
        result["capabilities"]["clientProfiles"] = serde_json::json!(["codex"]);
        result["capabilities"]["sdkFeatures"]["accountAutomation"] = serde_json::json!(1);
        *self
            .config
            .write()
            .map_err(|_| PluginError::invalid("The provider configuration is unavailable."))? =
            Some((definition, proxy));
        Ok(result)
    }
    async fn auth_methods(&self, context: CallContext) -> PluginResult<Value> {
        self.inner.auth_methods(context).await
    }
    async fn discover_accounts(&self, context: CallContext) -> PluginResult<Value> {
        self.inner.discover_accounts(context).await
    }
    async fn accounts(&self, context: CallContext) -> PluginResult<Value> {
        self.inner.accounts(context).await
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        let subscription = nora_provider::adapter::is_oauth(context.credentials.as_ref());
        let mut models = self.inner.models(context).await?;
        crate::provider::fast::models(&mut models, subscription);
        Ok(models)
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        self.inner.usage(context).await
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        self.inner.authorize(params).await
    }
    async fn callback(&self, params: Value) -> PluginResult<Value> {
        self.inner.callback(params).await
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        self.inner.refresh(params).await
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        if request.subscription_accounts_enabled
            && nora_provider::adapter::is_oauth(request.credentials.as_ref())
        {
            crate::provider::codex_version::observe(
                request.caller_user_agent.as_deref(),
                request.metadata.get("callerVersion").map(String::as_str),
                request.metadata.get("callerOriginator").map(String::as_str),
            );
        }
        self.inner.fetch(request).await
    }
    async fn call(&self, method: &str, params: Value) -> PluginResult<Value> {
        if method == "warmup" {
            return self.warmup(&params).await;
        }
        if method == "modelMetadata" && params.get("clientProfile").is_some() {
            if params["clientProfile"] != "codex" {
                return Err(PluginError::unsupported(
                    "This client profile is not provided by OpenAI.",
                ));
            }
            let version = crate::provider::codex_version::current().await;
            return Ok(
                serde_json::json!({"clientProfiles":{"codex":crate::provider::codex_version::headers(&version)}}),
            );
        }
        if method == "prepareReset" {
            let (auth, client) = self.reset_context_for(&params, false).await?;
            return resets::prepare(&auth, &client).await;
        }
        if method != "consumeReset" {
            return self.inner.call(method, params).await;
        }
        let (auth, client) = self.reset_context(&params).await?;
        resets::consume_checked(&auth, &params, &client).await
    }
}

#[cfg(test)]
#[path = "../test/plugin.test.rs"]
mod tests;

#[cfg(test)]
#[path = "../test/client_observation.test.rs"]
mod client_observation_tests;

#[cfg(test)]
#[path = "../test/client_profile.test.rs"]
mod client_profile_tests;

#[cfg(test)]
#[path = "../test/warmup.test.rs"]
mod warmup_tests;
