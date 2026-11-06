use crate::{auth, body, http, models};
use async_trait::async_trait;
use nora_ai::{HttpClient, HttpMethod, HttpRequest, ReqwestHttpClient};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, PluginError,
    PluginResult, ProviderPlugin, API_VERSION,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::sync::Mutex;

#[derive(Clone)]
struct Config {
    directory: PathBuf,
    proxy: Option<NetworkProxy>,
    endpoints: auth::Endpoints,
}
#[derive(Default)]
pub(crate) struct ChatGPTAPI {
    config: RwLock<Option<Config>>,
    flows: Mutex<BTreeMap<String, Arc<auth::Flow>>>,
    rotations: auth::Rotations,
    pub(crate) client: Option<Arc<dyn HttpClient>>,
    #[cfg(test)]
    pub(crate) saved: Mutex<Vec<Value>>,
}
impl ChatGPTAPI {
    fn config(&self) -> PluginResult<Config> {
        self.config
            .read()
            .map_err(|_| PluginError::invalid("ChatGPT API settings are unavailable."))?
            .clone()
            .ok_or_else(|| PluginError::invalid("Initialize ChatGPT API before using it."))
    }
    fn client(&self, proxy: Option<&NetworkProxy>) -> PluginResult<Arc<dyn HttpClient>> {
        let config = self.config()?;
        if let Some(proxy) = proxy.or(config.proxy.as_ref()) {
            proxy.validate()?;
        }
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        Ok(Arc::new(ReqwestHttpClient::with_client(
            nora_provider_plugin_sdk::http_client(proxy.or(config.proxy.as_ref()))?,
        )))
    }
    fn params_client(&self, params: &Value) -> PluginResult<Arc<dyn HttpClient>> {
        let proxy: Option<NetworkProxy> =
            serde_json::from_value(params.get("networkProxy").cloned().unwrap_or(Value::Null))
                .map_err(|_| PluginError::invalid("ChatGPT API network settings are invalid."))?;
        self.client(proxy.as_ref())
    }
    async fn persist(&self, credentials: Value) -> PluginResult<()> {
        #[cfg(test)]
        if self.client.is_some() {
            self.saved.lock().await.push(credentials);
            return Ok(());
        }
        nora_provider_plugin_sdk::save_credentials(credentials).await
    }
}
#[async_trait]
impl ProviderPlugin for ChatGPTAPI {
    fn supports_account_revocation(&self) -> bool {
        true
    }
    async fn revoke_account(&self, context: CallContext) -> PluginResult<()> {
        let saved = context
            .credentials
            .ok_or_else(|| PluginError::authentication("Sign in with ChatGPT."))?;
        let client = self.client(context.network_proxy.as_ref())?;
        auth::revoke(client.as_ref(), &self.config()?.endpoints, &saved).await
    }
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        if params.api_version != API_VERSION
            || params.plugin_id != "chatgpt-api"
            || params.manifest["id"] != "chatgpt-api"
            || !params.data_directory.is_absolute()
        {
            return Err(PluginError::invalid(
                "This executable requires the ChatGPT API provider manifest.",
            ));
        }
        let proxy: Option<NetworkProxy> = serde_json::from_value(
            params
                .manifest
                .pointer("/contributes/provider/config/networkProxy")
                .cloned()
                .unwrap_or(Value::Null),
        )
        .map_err(|_| PluginError::invalid("ChatGPT API network settings are invalid."))?;
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        *self
            .config
            .write()
            .map_err(|_| PluginError::invalid("ChatGPT API settings are unavailable."))? =
            Some(Config {
                directory: params.data_directory,
                proxy,
                endpoints: auth::Endpoints::default(),
            });
        Ok(json!({"apiVersion":API_VERSION,"capabilities":{"persistent":true}}))
    }
    async fn auth_methods(&self, _: CallContext) -> PluginResult<Value> {
        Ok(json!([{"id":"oauth","type":"oauth","label":"Sign in with ChatGPT"}]))
    }
    async fn accounts(&self, context: CallContext) -> PluginResult<Value> {
        let Some(saved) = context.credentials else {
            return Ok(json!([]));
        };
        let meta = auth::metadata(&saved)?;
        let mut profile = json!({"id":context.account,"configured":true});
        for key in ["email", "plan", "name"] {
            if let Some(value) = saved[key].as_str().filter(|s| {
                !s.trim().is_empty() && s.len() <= 512 && !s.chars().any(char::is_control)
            }) {
                profile[key] = json!(value);
            }
        }
        if !meta.subject.is_empty() {
            profile["accountId"] = json!(meta.subject);
        }
        Ok(json!([profile]))
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        let method = params
            .get("authMethodID")
            .or_else(|| params.get("method"))
            .and_then(Value::as_str)
            .unwrap_or("oauth");
        if method != "oauth" {
            return Err(PluginError::unsupported(method));
        }
        let config = self.config()?;
        let client = self.params_client(&params)?;
        let (mut reply, flow) =
            auth::authorize(&config.endpoints, &config.directory, client).await?;
        let mut flows = self.flows.lock().await;
        flows.retain(|_, flow| flow.deadline > tokio::time::Instant::now());
        if flows.len() >= 32 {
            return Err(PluginError::invalid(
                "Too many ChatGPT sign-ins are waiting.",
            ));
        }
        let session = uuid::Uuid::new_v4().to_string();
        flows.insert(session.clone(), Arc::new(flow));
        reply["session"] = json!(session);
        Ok(reply)
    }
    async fn callback(&self, params: Value) -> PluginResult<Value> {
        let session = auth::text(&params, "session");
        let flow = self
            .flows
            .lock()
            .await
            .get(session)
            .cloned()
            .ok_or_else(|| PluginError::invalid("The ChatGPT sign-in expired. Start again."))?;
        if let Some(reply) = auth::poll(&flow, session, &params).await? {
            return Ok(reply);
        }
        let result = auth::callback(&flow).await;
        self.flows.lock().await.remove(session);
        Ok(json!({"credentials":result?,"configured":true}))
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let saved = params
            .get("credentials")
            .ok_or_else(|| PluginError::authentication("Sign in with ChatGPT."))?;
        let next = self
            .rotations
            .refresh(
                saved,
                self.params_client(&params)?,
                self.config()?.endpoints,
                params["force"] == true,
            )
            .await?;
        if &next != saved {
            self.persist(next.clone()).await?;
        }
        Ok(json!({"credentials":next}))
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        let Some(saved) = context.credentials else {
            return Ok(json!({"models":[],"source":"preset","allowUnknownModels":true}));
        };
        auth::metadata(&saved)?;
        let config = self.config()?;
        let client = self.client(context.network_proxy.as_ref())?;
        let (status, _, value) = http::json(
            http::send(
                client.as_ref(),
                HttpRequest::new(HttpMethod::Get, format!("{}/models", config.endpoints.api))
                    .with_header(
                        "authorization",
                        format!("Bearer {}", auth::text(&saved, "access")),
                    ),
                Duration::from_secs(30),
            )
            .await?,
            4 * 1024 * 1024,
        )
        .await?;
        if status != 200 {
            return Err(PluginError::transport(
                body::explain(status, &value)
                    .unwrap_or_else(|| format!("ChatGPT models returned HTTP {status}.")),
            ));
        }
        let supplemental = context
            .public_model_catalogs
            .get("codex")
            .cloned()
            .unwrap_or_else(models::codex_cache);
        let models = models::parse(&value, supplemental)?;
        Ok(
            json!({"models":models,"source":"remote","allowUnknownModels":false,"defaultAPI":"openai-responses","defaultBaseURL":auth::BASE}),
        )
    }
    async fn usage(&self, _: CallContext) -> PluginResult<Value> {
        Ok(
            json!({"available":false,"message":"This plan shares its five-hour limit with ChatGPT and other apps. OpenAI shows its usage at https://chatgpt.com/settings/usage.","links":[{"label":"ChatGPT usage","url":"https://chatgpt.com/settings/usage"}]}),
        )
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        let config = self.config()?;
        if request.method != "POST" || request.url != format!("{}/responses", config.endpoints.api)
        {
            return Err(PluginError::invalid(
                "A ChatGPT sign-in only uses OpenAI's Responses endpoint.",
            ));
        }
        let saved = request
            .credentials
            .as_ref()
            .ok_or_else(|| PluginError::authentication("Sign in with ChatGPT."))?;
        auth::metadata(saved)?;
        let client = self.client(request.network_proxy.as_ref())?;
        let bytes = body::prepare(&request.decode_body()?.unwrap_or_default());
        let mut headers = http::caller_headers(request.headers);
        headers.retain(|name, _| {
            !["session_id", "conversation_id"].contains(&name.to_ascii_lowercase().as_str())
        });
        if let Some(key) = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v["prompt_cache_key"].as_str().map(str::to_owned))
            .filter(|s| s.len() <= 16384 && !s.chars().any(char::is_control))
        {
            headers.insert("session-id".into(), key.clone());
            headers.insert("thread-id".into(), key);
        }
        headers.insert(
            "authorization".into(),
            format!("Bearer {}", auth::text(saved, "access")),
        );
        headers.insert("content-type".into(), "application/json".into());
        headers.insert("accept".into(), "text/event-stream".into());
        let response = http::send(
            client.as_ref(),
            HttpRequest {
                method: HttpMethod::Post,
                url: request.url,
                headers,
                body: Some(bytes),
            },
            Duration::from_secs(120),
        )
        .await?;
        if (200..300).contains(&response.status) {
            return Ok(http::relay(response));
        }
        let (status, mut headers, mut value) = http::json(response, 64 * 1024).await?;
        if let Some(explanation) = body::explain(status, &value) {
            if !value.is_object() {
                value = json!({"error":{}});
            }
            if !value["error"].is_object() {
                value["error"] = json!({});
            }
            value["error"]["message"] = json!(explanation);
        }
        let mut bytes = serde_json::to_string(&value).unwrap_or_default();
        for secret in [auth::text(saved, "access"), auth::text(saved, "refresh")] {
            if !secret.is_empty() {
                bytes = bytes.replace(secret, "[redacted]");
            }
        }
        headers.retain(|name, _| {
            !["content-length", "content-encoding"].contains(&name.to_ascii_lowercase().as_str())
        });
        Ok(FetchResponse::bytes(status, headers, bytes.into_bytes()))
    }
    async fn call(&self, method: &str, params: Value) -> PluginResult<Value> {
        if method != "cancelAuthorization" {
            return Err(PluginError::unsupported(method));
        }
        if let Some(flow) = self
            .flows
            .lock()
            .await
            .remove(auth::text(&params, "session"))
        {
            flow.cancel();
        }
        Ok(json!({"canceled":true}))
    }
}

#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
