use crate::{auth, http};
use async_trait::async_trait;
use nora_ai::{HttpClient, HttpMethod, HttpRequest, ReqwestHttpClient};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, PluginError,
    PluginResult, ProviderPlugin, API_VERSION,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::sync::Mutex;

#[derive(Clone)]
struct Config {
    proxy: Option<NetworkProxy>,
}
#[derive(Default)]
pub(crate) struct NousPortal {
    config: RwLock<Option<Config>>,
    flows: Mutex<BTreeMap<String, Arc<auth::DeviceFlow>>>,
    rotations: auth::Rotations,
    pub(crate) client: Option<Arc<dyn HttpClient>>,
    #[cfg(test)]
    pub(crate) saved: Mutex<Vec<Value>>,
}
impl NousPortal {
    fn client(&self, proxy: Option<&NetworkProxy>) -> PluginResult<Arc<dyn HttpClient>> {
        let config = self
            .config
            .read()
            .map_err(|_| PluginError::invalid("Nous Portal settings are unavailable."))?
            .clone()
            .ok_or_else(|| PluginError::invalid("Initialize Nous Portal before using it."))?;
        if let Some(proxy) = proxy.or(config.proxy.as_ref()) {
            proxy.validate()?;
        }
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        Ok(Arc::new(ReqwestHttpClient::with_client(
            nora_provider_network::http_client(proxy.or(config.proxy.as_ref()))?,
        )))
    }
    fn params_client(&self, params: &Value) -> PluginResult<Arc<dyn HttpClient>> {
        let proxy: Option<NetworkProxy> =
            serde_json::from_value(params.get("networkProxy").cloned().unwrap_or(Value::Null))
                .map_err(|_| PluginError::invalid("Nous Portal network settings are invalid."))?;
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
impl ProviderPlugin for NousPortal {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        if params.api_version != API_VERSION
            || params.plugin_id != "nous-portal"
            || params.manifest["id"] != "nous-portal"
            || !params.data_directory.is_absolute()
        {
            return Err(PluginError::invalid(
                "This executable requires the Nous Portal provider manifest.",
            ));
        }
        let proxy: Option<NetworkProxy> = serde_json::from_value(
            params
                .manifest
                .pointer("/config/networkProxy")
                .cloned()
                .unwrap_or(Value::Null),
        )
        .map_err(|_| PluginError::invalid("Nous Portal network settings are invalid."))?;
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        *self
            .config
            .write()
            .map_err(|_| PluginError::invalid("Nous Portal settings are unavailable."))? =
            Some(Config { proxy });
        Ok(json!({"apiVersion":API_VERSION,"capabilities":{"persistent":true}}))
    }
    async fn auth_methods(&self, _: CallContext) -> PluginResult<Value> {
        Ok(json!([{"id":"oauth","type":"oauth","label":"Nous Portal (device code)"}]))
    }
    async fn accounts(&self, context: CallContext) -> PluginResult<Value> {
        let Some(credentials) = context.credentials else {
            return Ok(json!([]));
        };
        auth::metadata(&credentials)?;
        let c = auth::claims(auth::text(&credentials, "access"));
        let mut profile = json!({"id":context.account,"configured":true});
        for key in ["email", "name"] {
            if let Some(v) = c[key]
                .as_str()
                .or_else(|| credentials[key].as_str())
                .filter(|s| !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control))
            {
                profile[key] = json!(v);
            }
        }
        if let Some(id) = c["sub"].as_str().filter(|s| {
            !s.is_empty() && s.len() <= 512 && s.trim() == *s && !s.chars().any(char::is_control)
        }) {
            profile["accountId"] = json!(id);
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
        let client = self.params_client(&params)?;
        let (mut reply, flow) = auth::device(client.as_ref()).await?;
        let mut flows = self.flows.lock().await;
        flows.retain(|_, flow| flow.deadline > tokio::time::Instant::now());
        if flows.len() >= 32 {
            return Err(PluginError::invalid(
                "Too many Nous Portal sign-ins are waiting.",
            ));
        }
        let session = uuid::Uuid::new_v4().to_string();
        flows.insert(session.clone(), Arc::new(flow));
        reply["session"] = json!(session);
        Ok(reply)
    }
    async fn callback(&self, params: Value) -> PluginResult<Value> {
        let flow = self
            .flows
            .lock()
            .await
            .get(auth::text(&params, "session"))
            .cloned()
            .ok_or_else(|| auth::error("expired_token", false, 0))?;
        let client = self.params_client(&params)?;
        let credentials = auth::callback(&flow, client).await?;
        Ok(json!({"credentials":credentials,"configured":true}))
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let old = params
            .get("credentials")
            .ok_or_else(|| auth::error("not_signed_in", true, 0))?;
        let next = self
            .rotations
            .refresh(old, self.params_client(&params)?, params["force"] == true)
            .await?;
        if &next != old {
            self.persist(next.clone()).await?;
        }
        Ok(json!({"credentials":next}))
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        let Some(saved) = context.credentials else {
            return Ok(
                json!({"source":"preset","models":[{"id":"Hermes-4-70B","name":"Hermes-4-70B","contextWindow":0,"maxTokens":0}],"allowUnknownModels":true}),
            );
        };
        auth::assert_current(&saved, auth::now(), 0)?;
        let m = auth::metadata(&saved)?;
        let client = self.client(context.network_proxy.as_ref())?;
        let (status, _, body) = http::json(
            http::send(
                client.as_ref(),
                HttpRequest::new(HttpMethod::Get, format!("{}/models", m.base_url))
                    .with_header("accept", "application/json")
                    .with_header(
                        "authorization",
                        format!("Bearer {}", auth::text(&saved, "access")),
                    ),
                Duration::from_secs(15),
            )
            .await?,
        )
        .await?;
        if !(200..300).contains(&status) {
            return Err(auth::error("models_unavailable", false, status));
        }
        let entries = body["data"]
            .as_array()
            .ok_or_else(|| auth::error("invalid_models_response", false, 0))?;
        if entries.len() > 4096 {
            return Err(auth::error("invalid_models_response", false, 0));
        }
        let mut models = Vec::new();
        for entry in entries {
            let id = entry["id"]
                .as_str()
                .filter(|s| {
                    !s.trim().is_empty() && s.len() <= 512 && !s.chars().any(char::is_control)
                })
                .ok_or_else(|| auth::error("invalid_models_response", false, 0))?;
            if models.iter().any(|m: &Value| m["id"] == id) {
                continue;
            }
            models.push(json!({"id":id,"name":id,"provider":"nous-portal","api":"openai-completions","baseURL":m.base_url,"input":["text"],"contextWindow":0,"maxTokens":0,"reasoning":false,"toolCalling":true,"info":{"chat":true}}));
        }
        Ok(
            json!({"models":models,"source":"remote","authoritativeEmpty":entries.is_empty(),"allowUnknownModels":false,"defaultAPI":"openai-completions","defaultBaseURL":m.base_url}),
        )
    }
    async fn usage(&self, _: CallContext) -> PluginResult<Value> {
        Ok(
            json!({"available":false,"message":"Nous Portal does not provide account usage through this sign-in.","links":[{"label":"Nous Portal","url":auth::PORTAL}]}),
        )
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        let path = target(&request.url)?;
        let saved = request
            .credentials
            .as_ref()
            .ok_or_else(|| auth::error("not_signed_in", true, 0))?;
        auth::assert_current(saved, auth::now(), 0)?;
        let client = self.client(request.network_proxy.as_ref())?;
        let body = request.decode_body()?;
        let mut headers = http::caller_headers(request.headers);
        headers.insert(
            "authorization".into(),
            format!("Bearer {}", auth::text(saved, "access")),
        );
        let method = match request.method.as_str() {
            "GET" => HttpMethod::Get,
            "POST" => HttpMethod::Post,
            "PUT" => HttpMethod::Put,
            "PATCH" => HttpMethod::Patch,
            "DELETE" => HttpMethod::Delete,
            _ => {
                return Err(PluginError::invalid(
                    "The Nous Portal request method is unsupported.",
                ))
            }
        };
        let response = http::send(
            client.as_ref(),
            HttpRequest {
                method,
                url: format!("{}{path}", auth::metadata(saved)?.base_url),
                headers,
                body,
            },
            Duration::from_secs(120),
        )
        .await?;
        let mut response = http::relay(response);
        response
            .headers
            .insert("x-magpie-sign-in".into(), "kept".into());
        Ok(response)
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
            let _ = flow.canceled.send(true);
        }
        Ok(json!({"canceled":true}))
    }
}
pub(crate) fn target(value: &str) -> PluginResult<String> {
    let url = url::Url::parse(value)
        .map_err(|_| auth::error("refusing_inference_destination", false, 0))?;
    let origin = url.origin().ascii_serialization();
    if ![
        "https://inference-api.nousresearch.com",
        "https://welcome-api.nousresearch.com",
    ]
    .contains(&origin.as_str())
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || ![
            "/v1/chat/completions",
            "/v1/completions",
            "/v1/embeddings",
            "/v1/models",
        ]
        .contains(&url.path())
    {
        return Err(auth::error("refusing_inference_destination", false, 0));
    }
    Ok(format!(
        "{}{}",
        &url.path()[3..],
        url.query().map(|q| format!("?{q}")).unwrap_or_default()
    ))
}

#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
