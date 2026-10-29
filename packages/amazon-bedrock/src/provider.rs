use crate::{config::Account, request};
use async_trait::async_trait;
use nora_ai::{HttpClient, HttpMethod, HttpRequest, Model, ModelsCatalog};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, PluginError, PluginResult,
    ProviderPlugin,
};
use nora_provider_subscription_core::{self as core, Core};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::sync::Mutex;

#[derive(Default)]
struct Fit {
    other: bool,
    betas: BTreeSet<String>,
}
pub struct Bedrock {
    core: Core,
    mantle: bool,
    fit: Mutex<BTreeMap<String, Fit>>,
}
impl Bedrock {
    pub fn runtime() -> Self {
        Self {
            core: Core::new("amazon-bedrock"),
            mantle: false,
            fit: Mutex::default(),
        }
    }
    pub fn mantle() -> Self {
        Self {
            core: Core::new("amazon-bedrock-mantle"),
            mantle: true,
            fit: Mutex::default(),
        }
    }
    pub fn with_dependencies(
        mantle: bool,
        client: Arc<dyn HttpClient>,
        store: Arc<dyn core::CredentialStore>,
    ) -> Self {
        Self {
            core: Core::with_dependencies(
                if mantle {
                    "amazon-bedrock-mantle"
                } else {
                    "amazon-bedrock"
                },
                client,
                store,
            ),
            mantle,
            fit: Mutex::default(),
        }
    }
    fn catalog() -> &'static ModelsCatalog {
        static CATALOG: OnceLock<ModelsCatalog> = OnceLock::new();
        CATALOG.get_or_init(|| ModelsCatalog::bundled().unwrap_or_default())
    }
    fn model(&self, id: &str, entry: &Value, account: &Account, source: &str) -> Value {
        let catalog = Self::catalog();
        let mut model = catalog
            .model("amazon-bedrock", id)
            .cloned()
            .unwrap_or_else(|| {
                let mut model = Model::new(id, "openai-completions", self.core.id);
                model.context_window = 0;
                model.max_tokens = 0;
                model
            });
        let (api, base) = request::route(id, account, self.mantle);
        model.api = api.into();
        model.base_url = base;
        model.provider = self.core.id.into();
        model.info.catalog_source = Some(source.into());
        model.info.pricing_known = Some(catalog.model("amazon-bedrock", id).is_some_and(|known| {
            known.info.pricing_known == Some(true)
                || known.info.free == Some(true)
                || [
                    known.cost.input,
                    known.cost.output,
                    known.cost.cache_read,
                    known.cost.cache_write,
                ]
                .iter()
                .any(|cost| *cost > 0.0)
        }));
        if let Some(name) = entry
            .get("name")
            .or_else(|| entry.get("display_name"))
            .and_then(core::text)
        {
            model.name = name.into();
        }
        if let Some(limit) = entry
            .get("context_length")
            .or_else(|| entry.get("context_window"))
            .and_then(Value::as_u64)
            .filter(|value| *value > 0)
        {
            model.context_window = limit;
        }
        if let Some(limit) = entry
            .get("max_output_tokens")
            .and_then(Value::as_u64)
            .filter(|value| *value > 0)
        {
            model.max_tokens = limit;
        }
        if account.aws.is_some() {
            model
                .provider_options
                .insert("region".into(), json!(account.region).into());
        }
        serde_json::to_value(model).unwrap_or(Value::Null)
    }
    fn listing(&self, models: Vec<Value>, account: &Account, source: &str) -> Value {
        let (api, base) = request::route("", account, self.mantle);
        let mut listing = core::listing(models, api, &base, source);
        let patterns: &[(&str, &str)] = if account.aws.is_some() {
            &[("*", "")]
        } else {
            &[
                ("*anthropic.claude*", "anthropic.claude"),
                ("*openai.gpt-oss*", "openai.gpt-oss"),
                ("*openai.gpt-*", "openai.gpt-6"),
                ("xai.*", "xai.grok"),
                ("*", ""),
            ]
        };
        listing["modelTemplates"] = Value::Array(
            patterns
                .iter()
                .map(|(pattern, sample)| {
                    let (api, base) = request::route(sample, account, self.mantle);
                    let mut model = json!({
                        "api":api,"baseUrl":base,"contextWindow":0,"maxTokens":0,
                        "input":["text"],"info":{"catalogSource":"preset","pricingKnown":false}
                    });
                    if account.aws.is_some() {
                        model["providerOptions"] = json!({"region":account.region});
                    }
                    json!({"pattern":pattern,"model":model})
                })
                .collect(),
        );
        listing
    }
    async fn models_with(
        &self,
        context: CallContext,
        client: &dyn HttpClient,
    ) -> PluginResult<Value> {
        let manifest = self.core.config()?.manifest;
        let fallback = json!({"key":"unconnected","region":"us-east-1"});
        let saved = context.credentials.as_ref().unwrap_or(&fallback);
        let account = Account::saved(saved, &manifest, self.mantle)?;
        let mut models = Vec::new();
        if !self.mantle {
            let ids = if account.aws.is_some() {
                Self::catalog()
                    .models_for("amazon-bedrock")
                    .iter()
                    .map(|model| json!({"id":model.id}))
                    .collect()
            } else {
                manifest["contributes"]["provider"]["models"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
            };
            for entry in ids {
                if let Some(id) = entry
                    .as_str()
                    .or_else(|| entry.get("id").and_then(core::text))
                {
                    models.push(self.model(id, &entry, &account, "preset"));
                }
            }
            return Ok(self.listing(models, &account, "preset"));
        }
        if context.credentials.is_none() {
            return Ok(self.listing(models, &account, "preset"));
        }
        let mut after = String::new();
        let mut seen = BTreeSet::new();
        let mut cursors = BTreeSet::new();
        for _ in 0..40 {
            let mut url = url::Url::parse(&format!("{}/v1/models", account.root(true)))
                .map_err(|_| PluginError::invalid("The Bedrock region is invalid."))?;
            if !after.is_empty() {
                url.query_pairs_mut().append_pair("after", &after);
            }
            let payload = core::checked(
                client,
                HttpRequest::new(HttpMethod::Get, url.as_str())
                    .with_header("authorization", format!("Bearer {}", account.key)),
            )
            .await?;
            let entries = payload
                .get("data")
                .and_then(Value::as_array)
                .ok_or_else(|| PluginError::transport("Mantle did not return a model list."))?;
            for entry in entries {
                let Some(id) = entry
                    .get("id")
                    .and_then(core::text)
                    .filter(|id| id.len() <= 1024 && !id.chars().any(char::is_control))
                else {
                    continue;
                };
                if seen.insert(id.to_owned()) {
                    models.push(self.model(id, entry, &account, "remote"));
                }
                if models.len() > 4096 {
                    return Err(PluginError::transport(
                        "The Mantle model list is too large.",
                    ));
                }
            }
            if payload["has_more"] != true {
                return Ok(self.listing(models, &account, "remote"));
            }
            after = payload
                .get("last_id")
                .and_then(core::text)
                .filter(|id| id.len() <= 1024)
                .ok_or_else(|| PluginError::transport("Mantle model pagination is incomplete."))?
                .to_owned();
            if !cursors.insert(after.clone()) {
                return Err(PluginError::transport("Mantle repeated a model page."));
            }
        }
        Err(PluginError::transport(
            "Mantle model pagination exceeded its limit.",
        ))
    }
    async fn fetch_with(
        &self,
        request: FetchRequest,
        client: &dyn HttpClient,
    ) -> PluginResult<FetchResponse> {
        let manifest = self.core.config()?.manifest;
        let saved = request
            .credentials
            .as_ref()
            .ok_or_else(|| PluginError::authentication("Connect an Amazon Bedrock account."))?;
        let account = Account::saved(saved, &manifest, self.mantle)?;
        let body = core::request_body(&request)?;
        let model = body.get("model").and_then(core::text).unwrap_or("");
        let scope = format!("{}:{model}", core::fingerprint(saved));
        for attempt in 0..3 {
            let (other, betas) = {
                let fitted = self.fit.lock().await;
                fitted
                    .get(&scope)
                    .map(|fit| (fit.other, fit.betas.clone()))
                    .unwrap_or_default()
            };
            let upstream = request::prepare(&request, &account, self.mantle, other, &betas)?;
            let secrets = nora_provider::transport::request_secrets(&upstream);
            let response = tokio::time::timeout(Duration::from_secs(60), client.stream(upstream))
                .await
                .map_err(|_| PluginError::transport("Bedrock did not start responding in time."))?
                .map_err(|_| {
                    PluginError::transport("The Bedrock endpoint could not be reached.")
                })?;
            if response.status != 400 && response.status != 404 {
                return nora_provider::transport::finish_response(response, secrets).await;
            }
            let status = response.status;
            let mut headers = response.headers;
            let text = nora_ai::collect_http_body_text_limited(response.body, 64 * 1024)
                .await
                .map_err(|_| {
                    PluginError::transport("The Bedrock error response was interrupted.")
                })?;
            let lower = text.to_ascii_lowercase();
            let refused = request::refused_betas(&text);
            let wrong = self.mantle
                && !request.url.contains("/anthropic/")
                && (lower.contains("isn't supported on this route")
                    || lower.contains("is not supported on this route")
                    || lower.contains("not supported on this route"));
            let fresh = wrong && attempt == 0 || refused.iter().any(|beta| !betas.contains(beta));
            if attempt < 2 && fresh {
                let mut fitted = self.fit.lock().await;
                if fitted.len() >= 1024 && !fitted.contains_key(&scope) {
                    fitted.clear();
                }
                let fit = fitted.entry(scope.clone()).or_default();
                if wrong {
                    fit.other = !other;
                }
                fit.betas.extend(refused);
                continue;
            }
            headers.retain(|name, _| {
                ![
                    "content-length",
                    "content-encoding",
                    "transfer-encoding",
                    "set-cookie",
                ]
                .contains(&name.to_ascii_lowercase().as_str())
            });
            return Ok(FetchResponse::bytes(
                status,
                headers,
                nora_provider::transport::redact(text, &secrets).into_bytes(),
            ));
        }
        Err(PluginError::transport(
            "The Bedrock request could not be fitted to its endpoint.",
        ))
    }
}
#[async_trait]
impl ProviderPlugin for Bedrock {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        self.core.initialize(params)
    }
    async fn auth_methods(&self, _: CallContext) -> PluginResult<Value> {
        Ok(self.core.config()?.manifest["contributes"]["provider"]["authMethods"].clone())
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        Ok(
            json!({"ok":true,"credentials":Account::from_inputs(&params,&self.core.config()?.manifest,self.mantle)?}),
        )
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        let client = self.core.client(context.network_proxy.as_ref())?;
        self.models_with(context, client.as_ref()).await
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        let client = self.core.client(request.network_proxy.as_ref())?;
        self.fetch_with(request, client.as_ref()).await
    }
    async fn usage(&self, _: CallContext) -> PluginResult<Value> {
        Ok(
            json!({"available":false,"message":"AWS bills Bedrock usage separately. This API does not expose a subscription balance or reset time."}),
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
