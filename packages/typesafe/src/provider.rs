use async_trait::async_trait;
use futures::StreamExt;
use nora_ai::{HttpClient, HttpRequest, ReqwestHttpClient};
use nora_provider::{definition::Definition, transport, BundledProvider, ProviderAdapter};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, PluginError,
    PluginResult, ProviderPlugin, API_VERSION,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::RwLock;
use std::time::Duration;
use url::Url;

const MAX_REQUEST: usize = 16 * 1024 * 1024;
const MAX_RESPONSE: usize = 1024 * 1024;
const MAX_MODELS: usize = 4096;

#[derive(Debug)]
struct Identity;
impl ProviderAdapter for Identity {
    fn id(&self) -> Option<&'static str> {
        Some("typesafe")
    }
}

pub struct Typesafe {
    inner: BundledProvider,
    definition: RwLock<Option<Definition>>,
    proxy: RwLock<Option<NetworkProxy>>,
}

impl Default for Typesafe {
    fn default() -> Self {
        Self {
            inner: BundledProvider::new(Identity),
            definition: RwLock::default(),
            proxy: RwLock::default(),
        }
    }
}

impl Typesafe {
    fn definition(&self) -> PluginResult<Definition> {
        self.definition
            .read()
            .map_err(|_| {
                PluginError::invalid("The decision provider configuration is unavailable.")
            })?
            .clone()
            .ok_or_else(|| PluginError::invalid("Initialize the decision provider first."))
    }
    fn effective_proxy(
        &self,
        supplied: Option<NetworkProxy>,
    ) -> PluginResult<Option<NetworkProxy>> {
        let configured = self
            .proxy
            .read()
            .map_err(|_| PluginError::invalid("The decision provider proxy is unavailable."))?;
        let proxy = supplied.or_else(|| configured.clone());
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        Ok(proxy)
    }
}

#[async_trait]
impl ProviderPlugin for Typesafe {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        let definition = Definition::parse(params.manifest.clone())?;
        if params.api_version != API_VERSION
            || definition.id != "typesafe"
            || definition.api != "systemone"
        {
            return Err(PluginError::invalid(
                "This executable requires a TypeSafe SystemOne manifest.",
            ));
        }
        let proxy: Option<NetworkProxy> = serde_json::from_value(
            params
                .manifest
                .pointer("/config/networkProxy")
                .cloned()
                .unwrap_or(Value::Null),
        )
        .map_err(|_| PluginError::invalid("The decision provider proxy is invalid."))?;
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        let value = self.inner.initialize(params).await?;
        *self.definition.write().map_err(|_| {
            PluginError::invalid("The decision provider configuration is unavailable.")
        })? = Some(definition);
        *self
            .proxy
            .write()
            .map_err(|_| PluginError::invalid("The decision provider proxy is unavailable."))? =
            proxy;
        Ok(value)
    }
    async fn auth_methods(&self, context: CallContext) -> PluginResult<Value> {
        self.inner.auth_methods(context).await
    }
    async fn accounts(&self, context: CallContext) -> PluginResult<Value> {
        self.inner.accounts(context).await
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        self.inner.authorize(params).await
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        self.inner.refresh(params).await
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        self.inner.usage(context).await
    }
    async fn models(&self, mut context: CallContext) -> PluginResult<Value> {
        let definition = self.definition()?;
        context.network_proxy = self.effective_proxy(context.network_proxy)?;
        let client = ReqwestHttpClient::with_client(nora_provider_plugin_sdk::http_client(
            context.network_proxy.as_ref(),
        )?)
        .with_observer_fn(|| async {
            nora_provider_plugin_sdk::before_http_request()
                .await
                .map_err(|error| nora_ai::HttpClientError::Transport(error.to_string()))
        });
        list_models(&definition, context, &client).await
    }
    async fn fetch(&self, mut request: FetchRequest) -> PluginResult<FetchResponse> {
        let definition = self.definition()?;
        request.network_proxy = self.effective_proxy(request.network_proxy)?;
        let client = ReqwestHttpClient::with_client(nora_provider_plugin_sdk::http_client(
            request.network_proxy.as_ref(),
        )?)
        .with_observer_fn(|| async {
            nora_provider_plugin_sdk::before_http_request()
                .await
                .map_err(|error| nora_ai::HttpClientError::Transport(error.to_string()))
        });
        fetch(&definition, request, &client).await
    }
}

fn require_api(credentials: Option<&Value>) -> PluginResult<&Value> {
    credentials
        .filter(|value| nora_provider::auth::is_api(value))
        .ok_or_else(|| PluginError::authentication("Connect a TypeSafe API key."))
}

fn endpoint(
    definition: &Definition,
    credentials: Option<&Value>,
    suffix: &str,
) -> PluginResult<Url> {
    let base = nora_provider::models::effective_endpoint(definition, credentials);
    let mut target = nora_provider::discovery::validated_url(&base)?;
    if target.query().is_some() || target.fragment().is_some() {
        return Err(PluginError::invalid(
            "The SystemOne base address cannot contain a query or fragment.",
        ));
    }
    target.set_path(&format!("{}/{suffix}", target.path().trim_end_matches('/')));
    Ok(target)
}

pub(crate) fn marked(mut value: Value, definition: &Definition, base: &str, source: &str) -> Value {
    value["provider"] = json!(definition.id);
    value["api"] = json!("systemone");
    value["baseUrl"] = json!(base);
    if !value.get("info").is_some_and(Value::is_object) {
        value["info"] = json!({});
    }
    value["info"]["decision"] = json!(true);
    value["info"]["chat"] = json!(false);
    value["info"]["toolCalling"] = json!(false);
    value["info"]["catalogSource"] = json!(source);
    value
}

async fn list_models(
    definition: &Definition,
    context: CallContext,
    client: &dyn HttpClient,
) -> PluginResult<Value> {
    let base = nora_provider::models::effective_endpoint(definition, context.credentials.as_ref());
    let Some(credentials) = context.credentials.as_ref() else {
        return Ok(
            json!({"models":definition.models.iter().cloned().map(|model|marked(model,definition,&base,"preset")).collect::<Vec<_>>(),"source":"preset","allowUnknownModels":true,"defaultAPI":"systemone","defaultBaseURL":base}),
        );
    };
    require_api(Some(credentials))?;
    let request = transport::build_request(
        definition,
        FetchRequest {
            caller_user_agent: None,
            constraints: Default::default(),
            subscription_accounts_enabled: context.subscription_accounts_enabled,
            account: context.account,
            credentials: Some(credentials.clone()),
            network_proxy: context.network_proxy,
            url: endpoint(definition, Some(credentials), "models")?.into(),
            method: "GET".into(),
            headers: BTreeMap::from([("accept".into(), "application/json".into())]),
            metadata: BTreeMap::new(),
            body: None,
            body_file: None,
        },
    )
    .await?;
    let (status, _, body) = exchange(client, request, Duration::from_secs(20)).await?;
    if status != 200 {
        return Err(PluginError::transport(format!(
            "The SystemOne model list returned HTTP {status}."
        )));
    }
    let value: Value = serde_json::from_slice(&body).map_err(|_| {
        PluginError::transport("The decision provider returned an invalid model list.")
    })?;
    let entries = value
        .get("data")
        .or_else(|| value.get("models"))
        .or_else(|| value.is_array().then_some(&value))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            PluginError::transport("The decision provider returned an invalid model list.")
        })?;
    if entries.len() > MAX_MODELS {
        return Err(PluginError::transport(
            "The decision provider returned too many models.",
        ));
    }
    let mut seen = BTreeSet::new();
    let mut models = Vec::new();
    for entry in entries {
        let id = entry
            .as_str()
            .or_else(|| entry.get("id").and_then(Value::as_str))
            .or_else(|| entry.get("name").and_then(Value::as_str))
            .map(str::trim)
            .filter(|id| !id.is_empty() && id.len() <= 1024 && !id.chars().any(char::is_control));
        let Some(id) = id else {
            return Err(PluginError::transport(
                "The decision provider returned an invalid model ID.",
            ));
        };
        if !seen.insert(id.to_owned()) {
            continue;
        }
        let mut model = json!({"id":id,"name":entry.get("display_name").or_else(||entry.get("displayName")).or_else(||entry.get("name")).and_then(Value::as_str).unwrap_or(id)});
        for (target, keys) in [
            (
                "contextWindow",
                &["context_length", "context_window", "max_context_window"][..],
            ),
            ("maxTokens", &["max_output_tokens", "max_tokens"][..]),
        ] {
            if let Some(number) = keys
                .iter()
                .find_map(|key| entry.get(*key).and_then(Value::as_u64))
                .filter(|number| *number > 0)
            {
                model[target] = json!(number);
            }
        }
        if let Some(input) = entry
            .pointer("/modalities/input")
            .or_else(|| entry.get("input_modalities"))
            .and_then(Value::as_array)
        {
            model["input"] = json!(input
                .iter()
                .filter_map(Value::as_str)
                .filter(|input| matches!(*input, "text" | "image" | "audio" | "video" | "pdf"))
                .collect::<Vec<_>>());
        }
        if let Some(cost) = entry.get("cost").and_then(Value::as_object) {
            let cost = cost
                .iter()
                .filter(|(key, value)| {
                    matches!(key.as_str(), "input" | "output")
                        && value
                            .as_f64()
                            .is_some_and(|number| number.is_finite() && number >= 0.0)
                })
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<serde_json::Map<_, _>>();
            if !cost.is_empty() {
                model["cost"] = Value::Object(cost);
            }
        }
        models.push(marked(model, definition, &base, "remote"));
    }
    Ok(
        json!({"models":models,"source":"remote","allowUnknownModels":false,"defaultAPI":"systemone","defaultBaseURL":base}),
    )
}

pub(crate) fn validate_body(request: &FetchRequest) -> PluginResult<Vec<u8>> {
    if request
        .body_file
        .as_ref()
        .is_some_and(|file| file.length > MAX_REQUEST as u64)
        || request
            .body
            .as_ref()
            .is_some_and(|body| body.len() > MAX_REQUEST.div_ceil(3) * 4)
    {
        return Err(PluginError::invalid(
            "A SystemOne request cannot exceed 16 MiB.",
        ));
    }
    let bytes = request
        .decode_body()?
        .ok_or_else(|| PluginError::invalid("A SystemOne request body is required."))?;
    if bytes.len() > MAX_REQUEST {
        return Err(PluginError::invalid(
            "A SystemOne request cannot exceed 16 MiB.",
        ));
    }
    let body: Value = serde_json::from_slice(&bytes)
        .map_err(|_| PluginError::invalid("The SystemOne body must be JSON."))?;
    if body.get("bodyFile").is_some() || body.get("body_file").is_some() {
        return Err(PluginError::invalid(
            "Private body sources cannot be included in a SystemOne body.",
        ));
    }
    if body
        .get("model")
        .and_then(Value::as_str)
        .is_none_or(|model| {
            model.trim().is_empty() || model.len() > 1024 || model.chars().any(char::is_control)
        })
        || body.get("state").is_none()
        || body
            .get("questions")
            .and_then(Value::as_object)
            .is_none_or(|questions| questions.is_empty() || questions.len() > 128)
    {
        return Err(PluginError::invalid(
            "A SystemOne request needs model, state and 1 to 128 questions.",
        ));
    }
    for (name, question) in body["questions"].as_object().into_iter().flatten() {
        if name.is_empty()
            || name.len() > 256
            || name.chars().any(char::is_control)
            || !matches!(
                question.get("type").and_then(Value::as_str),
                Some("choice" | "score" | "noul")
            )
        {
            return Err(PluginError::invalid(
                "A SystemOne question requires a choice, score or noul type.",
            ));
        }
    }
    Ok(bytes)
}

async fn fetch(
    definition: &Definition,
    mut request: FetchRequest,
    client: &dyn HttpClient,
) -> PluginResult<FetchResponse> {
    let credentials = require_api(request.credentials.as_ref())?;
    let target = Url::parse(&request.url)
        .map_err(|_| PluginError::invalid("The decision request address is invalid."))?;
    let expected = endpoint(definition, Some(credentials), "systemone")?;
    if !request.method.eq_ignore_ascii_case("POST") || target != expected {
        return Err(PluginError::invalid(
            "This provider accepts only POST to its configured SystemOne endpoint.",
        ));
    }
    let bytes = validate_body(&request)?;
    request.body = None;
    request.body_file = None;
    request.headers.retain(|key, _| {
        !matches!(
            key.to_ascii_lowercase().as_str(),
            "content-type" | "content-encoding"
        )
    });
    request
        .headers
        .insert("content-type".into(), "application/json".into());
    let mut outgoing = transport::build_request(definition, request).await?;
    outgoing.body = Some(bytes);
    let (status, headers, bytes) = exchange(client, outgoing, Duration::from_secs(8)).await?;
    if status < 300 {
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
            PluginError::transport("The decision provider returned invalid SystemOne JSON.")
        })?;
        if !value.get("answers").is_some_and(Value::is_object) {
            return Err(PluginError::transport(
                "The decision provider returned no SystemOne answers.",
            ));
        }
    }
    Ok(FetchResponse::bytes(status, headers, bytes))
}

pub(crate) async fn exchange(
    client: &dyn HttpClient,
    request: HttpRequest,
    timeout: Duration,
) -> PluginResult<(u16, BTreeMap<String, String>, Vec<u8>)> {
    let secrets = transport::request_secrets(&request);
    let work = async {
        let mut response = client
            .stream(request)
            .await
            .map_err(|_| PluginError::transport("The decision provider could not be reached."))?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.body.next().await {
            let chunk = chunk
                .map_err(|_| PluginError::transport("The decision provider response stopped."))?;
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE {
                return Err(PluginError::transport(
                    "The SystemOne response exceeds 1 MiB.",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        if response.status >= 400 {
            bytes = transport::redact(String::from_utf8_lossy(&bytes).into_owned(), &secrets)
                .into_bytes();
        }
        let headers = response
            .headers
            .into_iter()
            .filter(|(name, _)| {
                !matches!(
                    name.to_ascii_lowercase().as_str(),
                    "content-length"
                        | "content-encoding"
                        | "set-cookie"
                        | "set-cookie2"
                        | "connection"
                        | "transfer-encoding"
                )
            })
            .map(|(name, value)| (name, transport::redact(value, &secrets)))
            .collect();
        Ok((response.status, headers, bytes))
    };
    tokio::time::timeout(timeout, work)
        .await
        .map_err(|_| PluginError::transport("The decision provider did not answer in time."))?
}

#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
