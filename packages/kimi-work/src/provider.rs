use async_trait::async_trait;
use nora_ai::{HttpClient, HttpMethod, HttpRequest, ReqwestHttpClient};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, PluginError,
    PluginResult, ProviderPlugin, API_VERSION,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;

#[path = "models.rs"]
mod models;
#[path = "session.rs"]
mod session;
#[path = "usage.rs"]
mod usage;

const BASE: &str = "https://agent-gw.kimi.com/coding/v1";
const MODELS: &str = "https://agent-gw.kimi.com/coding/v1/models";
const CHAT: &str = "https://agent-gw.kimi.com/coding/v1/chat/completions";
const MEMBERSHIP: &str =
    "https://www.kimi.com/apiv2/kimi.gateway.membership.v2.MembershipService/GetSubscriptionStats";
const USER_AGENT: &str = concat!("Nora Kimi Work provider/", env!("CARGO_PKG_VERSION"));

#[derive(Clone)]
struct Config {
    proxy: Option<NetworkProxy>,
    auth_methods: Value,
    preset_models: Value,
}

#[derive(Default)]
pub struct KimiWork {
    config: RwLock<Option<Config>>,
    client: Option<Arc<dyn HttpClient>>,
}

impl KimiWork {
    fn config(&self) -> PluginResult<Config> {
        self.config
            .read()
            .map_err(|_| PluginError::invalid("The Kimi Work configuration is unavailable."))?
            .clone()
            .ok_or_else(|| PluginError::invalid("Initialize Kimi Work before using it."))
    }

    fn client(&self, proxy: Option<&NetworkProxy>) -> PluginResult<Arc<dyn HttpClient>> {
        let config = self.config()?;
        let proxy = proxy.or(config.proxy.as_ref());
        if let Some(proxy) = proxy {
            proxy.validate()?;
        }
        if let Some(client) = &self.client {
            return Ok(Arc::clone(client));
        }
        let client = nora_provider_plugin_sdk::http_client(proxy)?;
        Ok(Arc::new(
            ReqwestHttpClient::with_client(client).with_observer_fn(|| async {
                nora_provider_plugin_sdk::before_http_request()
                    .await
                    .map_err(|error| nora_ai::HttpClientError::Transport(error.to_string()))
            }),
        ))
    }

    fn params_client(&self, params: &Value) -> PluginResult<Arc<dyn HttpClient>> {
        let proxy: Option<NetworkProxy> =
            serde_json::from_value(params.get("networkProxy").cloned().unwrap_or(Value::Null))
                .map_err(|_| {
                    PluginError::invalid("The Kimi Work network configuration is invalid.")
                })?;
        self.client(proxy.as_ref())
    }

    async fn models_with(
        &self,
        session: &session::Session,
        client: &dyn HttpClient,
    ) -> PluginResult<Value> {
        let payload = json_request(
            client,
            authorized(HttpMethod::Get, MODELS, &session.work_token),
            None,
        )
        .await?;
        models::parse(&payload)
    }

    async fn usage_with(
        &self,
        session: &session::Session,
        client: &dyn HttpClient,
    ) -> PluginResult<Value> {
        let payload = json_request(
            client,
            authorized(HttpMethod::Post, MEMBERSHIP, &session.membership_token)
                .with_header("content-type", "application/json")
                .with_header("connect-protocol-version", "1"),
            Some(b"{}".to_vec()),
        )
        .await?;
        Ok(usage::parse(&payload))
    }
}

fn authorized(method: HttpMethod, url: &str, token: &str) -> HttpRequest {
    HttpRequest::new(method, url)
        .with_header("authorization", format!("Bearer {token}"))
        .with_header("user-agent", USER_AGENT)
        .with_header("accept", "application/json")
}

fn sign_in_expired() -> PluginError {
    PluginError::authentication("The Kimi Work sign-in expired. Reconnect the account.")
}

async fn json_request(
    client: &dyn HttpClient,
    mut request: HttpRequest,
    body: Option<Vec<u8>>,
) -> PluginResult<Value> {
    request.body = body;
    tokio::time::timeout(Duration::from_secs(15), async {
        let response = client
            .stream(request)
            .await
            .map_err(|_| PluginError::transport("The Kimi Work service could not be reached."))?;
        if response.status == 401 {
            return Err(sign_in_expired());
        }
        if !(200..300).contains(&response.status) {
            return Err(PluginError::transport(format!(
                "The Kimi Work service returned HTTP {}.",
                response.status
            )));
        }
        let body = nora_ai::collect_http_body_text_limited(response.body, 2 * 1024 * 1024 + 1)
            .await
            .map_err(|_| {
                PluginError::transport("The Kimi Work response is incomplete or too large.")
            })?;
        if body.len() > 2 * 1024 * 1024 {
            return Err(PluginError::transport(
                "The Kimi Work response is too large.",
            ));
        }
        serde_json::from_str(&body)
            .map_err(|_| PluginError::transport("The Kimi Work response is invalid."))
    })
    .await
    .map_err(|_| PluginError::transport("The Kimi Work request timed out."))?
}

fn inference_request(
    request: FetchRequest,
    session: &session::Session,
) -> PluginResult<HttpRequest> {
    let target = url::Url::parse(&request.url)
        .map_err(|_| PluginError::invalid("The Kimi Work request address is invalid."))?;
    if target.as_str() != CHAT || request.method != "POST" {
        return Err(PluginError::invalid(
            "Kimi Work only accepts POST requests to its official chat endpoint.",
        ));
    }
    let body = request.decode_body()?;
    let mut headers = request
        .headers
        .into_iter()
        .filter(|(name, _)| {
            !matches!(
                name.to_ascii_lowercase().as_str(),
                "authorization"
                    | "user-agent"
                    | "content-type"
                    | "proxy-authorization"
                    | "x-api-key"
                    | "api-key"
                    | "cookie"
                    | "host"
                    | "content-length"
                    | "origin"
                    | "referer"
                    | "connection"
                    | "transfer-encoding"
            )
        })
        .collect::<BTreeMap<_, _>>();
    headers.insert(
        "authorization".into(),
        format!("Bearer {}", session.work_token),
    );
    headers.insert("user-agent".into(), USER_AGENT.into());
    headers.insert("content-type".into(), "application/json".into());
    Ok(HttpRequest {
        method: HttpMethod::Post,
        url: CHAT.into(),
        headers,
        body,
    })
}

#[async_trait]
impl ProviderPlugin for KimiWork {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        if params.api_version != API_VERSION
            || params.plugin_id != "kimi-work"
            || params.manifest["id"] != "kimi-work"
        {
            return Err(PluginError::invalid(
                "The Kimi Work provider identity is invalid.",
            ));
        }
        let base = params
            .manifest
            .get("baseURL")
            .and_then(Value::as_str)
            .and_then(|base| url::Url::parse(base).ok());
        if base
            .as_ref()
            .is_none_or(|base| base.as_str().trim_end_matches('/') != BASE)
        {
            return Err(PluginError::invalid(
                "Kimi Work requires its official Work gateway endpoint.",
            ));
        }
        let proxy: Option<NetworkProxy> = serde_json::from_value(
            params
                .manifest
                .pointer("/config/networkProxy")
                .cloned()
                .unwrap_or(Value::Null),
        )
        .map_err(|_| PluginError::invalid("The Kimi Work network configuration is invalid."))?;
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        *self
            .config
            .write()
            .map_err(|_| PluginError::invalid("The Kimi Work configuration is unavailable."))? =
            Some(Config {
                proxy,
                auth_methods: params
                    .manifest
                    .get("authMethods")
                    .cloned()
                    .unwrap_or_else(|| json!([])),
                preset_models: params
                    .manifest
                    .get("models")
                    .cloned()
                    .unwrap_or_else(|| json!([])),
            });
        Ok(json!({"apiVersion":API_VERSION}))
    }

    async fn auth_methods(&self, _context: CallContext) -> PluginResult<Value> {
        Ok(self.config()?.auth_methods)
    }

    async fn accounts(&self, context: CallContext) -> PluginResult<Value> {
        self.config()?;
        let Some(credentials) = context.credentials.as_ref() else {
            return Ok(json!([]));
        };
        session::current(Some(credentials))?;
        Ok(json!([{"id":context.account,"name":"Kimi Work","plan":"Kimi Work"}]))
    }

    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        let client = self.params_client(&params)?;
        let method = params
            .get("authMethodID")
            .or_else(|| params.get("method"))
            .and_then(Value::as_str)
            .ok_or_else(|| PluginError::invalid("Choose how to connect Kimi Work."))?;
        let inputs = params
            .get("inputs")
            .or_else(|| params.get("fields"))
            .unwrap_or(&params);
        let selected = if method == "desktop" {
            Some(
                inputs
                    .get("configPath")
                    .and_then(Value::as_str)
                    .filter(|path| !path.is_empty() && path.len() <= 16 * 1024)
                    .ok_or_else(|| {
                        PluginError::invalid("Select Kimi desktop's config.json explicitly.")
                    })?,
            )
        } else if method == "tokens" {
            None
        } else {
            return Err(PluginError::unsupported(method));
        };
        let session = if let Some(path) = selected {
            session::read_selected(Path::new(path))?
        } else {
            session::pasted(inputs)?
        };
        self.models_with(&session, client.as_ref()).await?;
        if selected.is_none() {
            self.usage_with(&session, client.as_ref()).await?;
        }
        Ok(json!({"ok":true,"credentials":session::credentials(&session, selected)}))
    }

    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        self.config()?;
        session::current(params.get("credentials"))?;
        Ok(json!({"changed":false}))
    }

    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        if context.credentials.is_none() {
            return Ok(
                json!({"models":self.config()?.preset_models,"source":"preset","allowUnknownModels":false,"defaultAPI":"openai-completions","defaultBaseURL":BASE}),
            );
        }
        let client = self.client(context.network_proxy.as_ref())?;
        let session = session::current(context.credentials.as_ref())?;
        self.models_with(&session, client.as_ref()).await
    }

    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let client = self.client(context.network_proxy.as_ref())?;
        let session = session::current(context.credentials.as_ref())?;
        self.usage_with(&session, client.as_ref()).await
    }

    async fn call(&self, method: &str, _params: Value) -> PluginResult<Value> {
        self.config()?;
        if method == "maintain" {
            return Ok(json!({"ok":true,"modelsChanged":false}));
        }
        Err(PluginError::unsupported(method))
    }

    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        let client = self.client(request.network_proxy.as_ref())?;
        let session = session::current(request.credentials.as_ref())?;
        let outgoing = inference_request(request, &session)?;
        let response = client
            .stream(outgoing)
            .await
            .map_err(|_| PluginError::transport("The Kimi Work endpoint could not be reached."))?;
        if response.status == 401 {
            return Err(sign_in_expired());
        }
        nora_provider::transport::finish_response(
            response,
            vec![session.work_token, session.membership_token],
        )
        .await
    }
}

#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
