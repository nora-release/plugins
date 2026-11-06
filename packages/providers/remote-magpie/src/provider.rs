use crate::{config::Account, models, transport, usage};
use async_trait::async_trait;
use nora_ai::{HttpClient, HttpMethod, ReqwestHttpClient};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, PluginError,
    PluginResult, ProviderPlugin, API_VERSION,
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
};
#[derive(Clone)]
struct Config {
    directory: PathBuf,
    proxy: Option<NetworkProxy>,
    manifest: Value,
}
#[derive(Default)]
pub(crate) struct Remote {
    config: RwLock<Option<Config>>,
    client: Option<Arc<dyn HttpClient>>,
}
impl Remote {
    fn config(&self) -> PluginResult<Config> {
        self.config
            .read()
            .map_err(|_| unavailable())?
            .clone()
            .ok_or_else(unavailable)
    }
    fn client(&self, proxy: Option<&NetworkProxy>) -> PluginResult<Arc<dyn HttpClient>> {
        if let Some(proxy) = proxy {
            proxy.validate()?;
        }
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        let config = self.config()?;
        let client = nora_provider_plugin_sdk::http_client(proxy.or(config.proxy.as_ref()))?;
        Ok(Arc::new(
            ReqwestHttpClient::with_client(client).with_observer_fn(|| async {
                nora_provider_plugin_sdk::before_http_request()
                    .await
                    .map_err(|error| nora_ai::HttpClientError::Transport(error.to_string()))
            }),
        ))
    }
    fn params_proxy(&self, params: &Value) -> PluginResult<Option<NetworkProxy>> {
        serde_json::from_value(params.get("networkProxy").cloned().unwrap_or(Value::Null))
            .map_err(|_| PluginError::invalid("The remote gateway network settings are invalid."))
    }
}
#[async_trait]
impl ProviderPlugin for Remote {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        if params.api_version != API_VERSION
            || params.plugin_id != "remote-magpie"
            || params.manifest["id"] != "remote-magpie"
        {
            return Err(PluginError::invalid(
                "This executable requires the Remote Magpie provider manifest.",
            ));
        }
        if !params.data_directory.is_absolute() {
            return Err(PluginError::invalid(
                "A private provider data directory is required.",
            ));
        }
        let proxy: Option<NetworkProxy> = serde_json::from_value(
            params
                .manifest
                .pointer("/contributes/provider/config/networkProxy")
                .cloned()
                .unwrap_or(Value::Null),
        )
        .map_err(|_| unavailable())?;
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        *self.config.write().map_err(|_| unavailable())? = Some(Config {
            directory: params.data_directory,
            proxy,
            manifest: params.manifest,
        });
        Ok(
            json!({"apiVersion":API_VERSION,"capabilities":{"models":true,"usage":true,"fetch":true}}),
        )
    }
    async fn auth_methods(&self, _: CallContext) -> PluginResult<Value> {
        Ok(self
            .config()?
            .manifest
            .pointer("/contributes/provider/authMethods")
            .cloned()
            .unwrap_or(json!([])))
    }
    async fn accounts(&self, _: CallContext) -> PluginResult<Value> {
        Ok(json!([]))
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        let method = params
            .get("authMethodID")
            .or_else(|| params.get("method"))
            .and_then(Value::as_str)
            .unwrap_or("apiKey");
        if method != "apiKey" {
            return Err(PluginError::unsupported("authorization method"));
        }
        let account = Account::inputs(&params)?;
        let proxy = self.params_proxy(&params)?;
        let client = self.client(proxy.as_ref())?;
        let remote = transport::read(
            client.as_ref(),
            &account,
            HttpMethod::Get,
            "/v1/models",
            true,
        )
        .await
        .map_err(transport::RemoteError::plugin)?;
        models::parse(&remote, &account)?;
        Ok(json!({"credentials":account.saved(),"configured":true}))
    }
    async fn refresh(&self, _: Value) -> PluginResult<Value> {
        Ok(json!({"changed":false}))
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        if context.credentials.is_none() {
            return Ok(json!({"models":[],"source":"preset","allowUnknownModels":false}));
        }
        let account = Account::credentials(context.credentials.as_ref())?;
        let client = self.client(context.network_proxy.as_ref())?;
        let remote = transport::read(
            client.as_ref(),
            &account,
            HttpMethod::Get,
            "/v1/models",
            true,
        )
        .await
        .map_err(transport::RemoteError::plugin)?;
        let mut result = models::parse(&remote, &account)?;
        if remote["data"].as_array().is_some_and(Vec::is_empty) {
            result["authoritativeEmpty"] = json!(true);
        }
        Ok(result)
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let account = Account::credentials(context.credentials.as_ref())?;
        let client = self.client(context.network_proxy.as_ref())?;
        usage::read(
            &self.config()?.directory,
            client.as_ref(),
            &account,
            &context.account,
            false,
        )
        .await
    }
    async fn fetch(&self, mut request: FetchRequest) -> PluginResult<FetchResponse> {
        let account = Account::credentials(request.credentials.as_ref())?;
        if request.network_proxy.is_none() {
            request.network_proxy = self.config()?.proxy;
        }
        let client = self.client(request.network_proxy.as_ref())?;
        transport::fetch(client.as_ref(), &account, request).await
    }
    async fn call(&self, method: &str, params: Value) -> PluginResult<Value> {
        let account = Account::credentials(params.get("credentials"))?;
        let local = params
            .get("account")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let proxy = self.params_proxy(&params)?;
        let client = self.client(proxy.as_ref())?;
        match method {
            "refreshRemoteCard" | "refreshUsage" => {
                usage::refresh(
                    &self.config()?.directory,
                    client.as_ref(),
                    &account,
                    local,
                    &params,
                )
                .await
            }
            "remoteHistory" => {
                usage::history(
                    client.as_ref(),
                    &account,
                    local,
                    params.get("days").and_then(Value::as_str).unwrap_or("7"),
                )
                .await
            }
            "remoteCards" => {
                usage::read(
                    &self.config()?.directory,
                    client.as_ref(),
                    &account,
                    local,
                    false,
                )
                .await
            }
            _ => Err(PluginError::unsupported(method)),
        }
    }
}
fn unavailable() -> PluginError {
    PluginError::invalid("Initialize the remote gateway provider before using it.")
}
#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
