use crate::{
    config::{reconnect, Config},
    credentials::{self, Source, Token},
};
use async_trait::async_trait;
use futures::StreamExt;
use nora_ai::{HttpClient, HttpMethod, HttpRequest, ReqwestHttpClient};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, PluginError,
    PluginResult, ProviderPlugin, API_VERSION,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, RwLock},
};
use tokio::sync::Mutex;
use url::Url;

struct CachedToken {
    token: Token,
    error: Option<PluginError>,
    failed_at: i64,
}

#[derive(Default)]
pub(crate) struct Vertex {
    manifest: RwLock<Option<Value>>,
    proxy: RwLock<Option<NetworkProxy>>,
    client: Option<Arc<dyn HttpClient>>,
    tokens: Mutex<BTreeMap<String, CachedToken>>,
}

impl Vertex {
    fn client(&self, supplied: Option<&NetworkProxy>) -> PluginResult<Arc<dyn HttpClient>> {
        if let Some(proxy) = supplied {
            proxy.validate()?;
        }
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        let default = self.proxy.read().map_err(|_| reconnect())?;
        Ok(Arc::new(
            ReqwestHttpClient::with_client(nora_provider_network::http_client(
                supplied.or(default.as_ref()),
            )?)
            .with_observer_fn(|| async {
                nora_provider_plugin_sdk::before_http_request()
                    .await
                    .map_err(|error| nora_ai::HttpClientError::Transport(error.to_string()))
            }),
        ))
    }

    async fn token(
        &self,
        config: &Config,
        client: &dyn HttpClient,
        force: bool,
    ) -> PluginResult<Token> {
        let source = credentials::read(config)?;
        if source.identity != config.identity {
            return Err(reconnect());
        }
        self.mint_cached(config, &source, client, force).await
    }

    async fn mint_cached(
        &self,
        config: &Config,
        source: &Source,
        client: &dyn HttpClient,
        force: bool,
    ) -> PluginResult<Token> {
        let key = format!(
            "{}\0{}\0{}",
            config.source_path.display(),
            source.stamp,
            config.impersonate
        );
        let mut cache = self.tokens.lock().await;
        if !force {
            if let Some(entry) = cache.get(&key) {
                if let Some(error) = &entry.error {
                    if credentials::now() - entry.failed_at < 2000 {
                        return Err(error.clone());
                    }
                } else if entry.token.expires > credentials::now() + 300_000 {
                    return Ok(entry.token.clone());
                }
            }
        }
        let result = credentials::mint(client, source, config).await;
        if cache.len() >= 32 {
            cache.retain(|_, entry| entry.token.expires > credentials::now());
            if cache.len() >= 32 {
                cache.clear();
            }
        }
        match result {
            Ok(token) => {
                cache.insert(
                    key,
                    CachedToken {
                        token: token.clone(),
                        error: None,
                        failed_at: 0,
                    },
                );
                Ok(token)
            }
            Err(error) => {
                cache.insert(
                    key,
                    CachedToken {
                        token: Token {
                            access: String::new(),
                            expires: 0,
                        },
                        error: Some(error.clone()),
                        failed_at: credentials::now(),
                    },
                );
                Err(error)
            }
        }
    }

    async fn authorize_with(&self, params: Value, client: &dyn HttpClient) -> PluginResult<Value> {
        if params
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("adc")
            != "adc"
        {
            return Err(PluginError::unsupported("authorization method"));
        }
        let mut config = Config::from_inputs(params.get("inputs").unwrap_or(&params))?;
        let source = credentials::read(&config)?;
        config.identity = source.identity.clone();
        self.mint_cached(&config, &source, client, false).await?;
        let mut saved = serde_json::to_value(&config).map_err(|_| reconnect())?;
        saved["type"] = json!("source");
        saved["authMethod"] = json!("adc");
        saved["configured"] = json!(true);
        saved["label"] = json!(config.project);
        if let Some(email) = source.value.get("client_email").and_then(Value::as_str) {
            saved["email"] = json!(email);
        }
        Ok(json!({"ok":true,"credentials":saved}))
    }

    async fn models_with(
        &self,
        context: CallContext,
        client: &dyn HttpClient,
    ) -> PluginResult<Value> {
        let config = Config::from_saved(context.credentials.as_ref())?;
        let token = self.token(&config, client, false).await?;
        let mut models = Vec::new();
        let mut seen = BTreeSet::new();
        let mut pages = BTreeSet::new();
        let mut page = String::new();
        for _ in 0..40 {
            let mut target = Url::parse(&format!("{}/v1/publishers/google/models", config.root()))
                .map_err(|_| reconnect())?;
            {
                let mut q = target.query_pairs_mut();
                q.append_pair("pageSize", "100")
                    .append_pair("listAllVersions", "true");
                if !page.is_empty() {
                    q.append_pair("pageToken", &page);
                }
            }
            let response = credentials::json_request(
                client,
                HttpRequest::new(HttpMethod::Get, target.as_str())
                    .with_header("authorization", format!("Bearer {}", token.access)),
            )
            .await?;
            let entries = response
                .get("publisherModels")
                .or_else(|| response.get("models"))
                .and_then(Value::as_array)
                .ok_or_else(|| PluginError::transport("Vertex did not return a model list."))?;
            for entry in entries {
                let Some(id) = entry
                    .get("name")
                    .or_else(|| entry.get("id"))
                    .and_then(Value::as_str)
                    .and_then(|id| id.rsplit('/').next())
                else {
                    continue;
                };
                if !id.starts_with("gemini-")
                    || id.len() > 256
                    || !id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b))
                    || !seen.insert(id.to_string())
                {
                    continue;
                }
                if models.len() >= 4096 {
                    return Err(PluginError::transport("Vertex returned too many models."));
                }
                let name = entry
                    .get("displayName")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty() && name.len() <= 512)
                    .unwrap_or(id);
                let mut model = json!({"id":id,"name":name,"api":"google-generative-ai","provider":"google-vertex","baseUrl":config.base(),"input":["text","image"],"reasoning":true,"info":{"catalogSource":"remote","chat":true,"toolCalling":true,"pricingKnown":false}});
                for (source, target) in [
                    ("inputTokenLimit", "contextWindow"),
                    ("outputTokenLimit", "maxTokens"),
                ] {
                    if let Some(value) = entry
                        .get(source)
                        .and_then(Value::as_u64)
                        .filter(|value| *value > 0)
                    {
                        model[target] = json!(value);
                    }
                }
                models.push(model);
            }
            page = response
                .get("nextPageToken")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if page.is_empty() {
                return Ok(
                    json!({"models":models,"source":"remote","allowUnknownModels":true,"defaultAPI":"google-generative-ai","defaultBaseURL":config.base()}),
                );
            }
            if page.len() > 4096 || !pages.insert(page.clone()) {
                return Err(PluginError::transport(
                    "Vertex returned an invalid model-list page token.",
                ));
            }
        }
        Err(PluginError::transport(
            "Vertex model pagination exceeded its limit.",
        ))
    }

    async fn fetch_with(
        &self,
        request: FetchRequest,
        client: &dyn HttpClient,
    ) -> PluginResult<FetchResponse> {
        let config = Config::from_saved(request.credentials.as_ref())?;
        let mut target = Url::parse(&request.url)
            .map_err(|_| PluginError::invalid("The Vertex request URL is invalid."))?;
        let base = Url::parse(&config.base()).map_err(|_| reconnect())?;
        if request.method != "POST"
            || target.origin() != base.origin()
            || !target.username().is_empty()
            || target.password().is_some()
            || target.fragment().is_some()
        {
            return Err(PluginError::invalid(
                "Vertex requests must use this account's project and location.",
            ));
        }
        let Some(tail) = target
            .path()
            .strip_prefix(&format!("{}/models/", base.path()))
        else {
            return Err(PluginError::invalid(
                "Vertex requests must use this account's model endpoint.",
            ));
        };
        let Some((model, action)) = tail.split_once(':') else {
            return Err(PluginError::invalid(
                "A Vertex model operation is required.",
            ));
        };
        if model.is_empty()
            || model.len() > 256
            || !model
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b))
            || !matches!(
                action,
                "generateContent"
                    | "streamGenerateContent"
                    | "countTokens"
                    | "embedContent"
                    | "batchEmbedContents"
            )
        {
            return Err(PluginError::unsupported("Vertex model operation"));
        }
        let mut query = Vec::new();
        for (key, value) in target.query_pairs() {
            match key.as_ref() {
                "key" => {}
                "alt" if value == "sse" => query.push((key.into_owned(), value.into_owned())),
                _ => {
                    return Err(PluginError::invalid(
                        "This Vertex query parameter is unsupported.",
                    ))
                }
            }
        }
        target.set_query(None);
        if !query.is_empty() {
            target.query_pairs_mut().extend_pairs(query);
        }
        let token = self.token(&config, client, false).await?;
        let mut upstream = HttpRequest::new(HttpMethod::Post, target.as_str())
            .with_header("authorization", format!("Bearer {}", token.access))
            .with_header("content-type", "application/json");
        upstream.headers.extend(config.headers.clone());
        for (name, value) in request.headers.iter().filter(|(name, _)| {
            matches!(
                name.to_ascii_lowercase().as_str(),
                "accept" | "x-goog-api-client"
            )
        }) {
            upstream.headers.insert(name.clone(), value.clone());
        }
        upstream.body = request.decode_body()?;
        let response = client
            .stream(upstream)
            .await
            .map_err(|_| PluginError::transport("The Vertex request could not be sent."))?;
        Ok(FetchResponse {
            status: response.status,
            headers: response.headers,
            body: Box::pin(response.body.map(|chunk| {
                chunk
                    .map(|chunk| chunk.to_vec())
                    .map_err(|_| PluginError::transport("The Vertex response was interrupted."))
            })),
        })
    }
}

#[async_trait]
impl ProviderPlugin for Vertex {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        if params.api_version != API_VERSION
            || params.plugin_id != "google-vertex"
            || params.manifest["id"] != "google-vertex"
        {
            return Err(PluginError::invalid(
                "This executable requires the Google Vertex AI manifest.",
            ));
        }
        let proxy: Option<NetworkProxy> = serde_json::from_value(
            params
                .manifest
                .pointer("/config/networkProxy")
                .cloned()
                .unwrap_or(Value::Null),
        )
        .map_err(|_| PluginError::invalid("The Vertex network settings are invalid."))?;
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        *self.proxy.write().map_err(|_| reconnect())? = proxy;
        *self.manifest.write().map_err(|_| reconnect())? = Some(params.manifest);
        Ok(
            json!({"apiVersion":API_VERSION,"capabilities":{"models":true,"usage":true,"fetch":true,"persistent":true}}),
        )
    }
    async fn auth_methods(&self, _: CallContext) -> PluginResult<Value> {
        Ok(self
            .manifest
            .read()
            .map_err(|_| reconnect())?
            .as_ref()
            .ok_or_else(reconnect)?["authMethods"]
            .clone())
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        let proxy: Option<NetworkProxy> =
            serde_json::from_value(params.get("networkProxy").cloned().unwrap_or(Value::Null))
                .map_err(|_| PluginError::invalid("The Vertex network settings are invalid."))?;
        let client = self.client(proxy.as_ref())?;
        self.authorize_with(params, client.as_ref()).await
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        let client = self.client(context.network_proxy.as_ref())?;
        self.models_with(context, client.as_ref()).await
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        let client = self.client(request.network_proxy.as_ref())?;
        self.fetch_with(request, client.as_ref()).await
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let proxy: Option<NetworkProxy> =
            serde_json::from_value(params.get("networkProxy").cloned().unwrap_or(Value::Null))
                .map_err(|_| PluginError::invalid("The Vertex network settings are invalid."))?;
        let client = self.client(proxy.as_ref())?;
        let config = Config::from_saved(params.get("credentials"))?;
        self.token(&config, client.as_ref(), true).await?;
        Ok(json!({"changed":false}))
    }
    async fn usage(&self, _: CallContext) -> PluginResult<Value> {
        Ok(
            json!({"available":false,"reason":"Google Cloud bills this project. Vertex does not expose a subscription usage or account balance endpoint."}),
        )
    }
    async fn call(&self, method: &str, _: Value) -> PluginResult<Value> {
        if method == "maintain" {
            Ok(json!({"ok":true}))
        } else {
            Err(PluginError::unsupported(method))
        }
    }
}

#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
