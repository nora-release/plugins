//! Workers AI account transport and discovery, adapted from magpie-community (MIT).
use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use nora_ai::{HttpClient, HttpMethod, HttpRequest, Model, ReqwestHttpClient};
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

const ID: &str = "cloudflare-workers-ai";
const DEFAULT_BASE: &str =
    "https://api.cloudflare.com/client/v4/accounts/${CLOUDFLARE_ACCOUNT_ID}/ai/v1";
const CATALOG: &str = "https://models.dev/api.json";
const PAID: &[&str] = &[
    "@cf/moonshotai/kimi-k2.6",
    "@cf/moonshotai/kimi-k2.7-code",
    "@cf/zai-org/glm-5.2",
    "@cf/zai-org/glm-5.3",
    "@cf/zai-org/glm-5.3-flash",
    "@cf/deepseek-ai/deepseek-v4-flash-0731",
    "@cf/deepseek-ai/deepseek-v4.1-flash-1017",
    "@cf/deepseek-ai/deepseek-v4-pro-0813",
];
#[derive(Clone)]
struct Config {
    manifest: Value,
    proxy: Option<NetworkProxy>,
    directory: PathBuf,
    template: String,
    account: String,
    ttl: i64,
}
#[derive(Clone, Default)]
struct Listing {
    models: Vec<Model>,
    at: Option<i64>,
}
#[derive(Default)]
pub struct CloudflareWorkersAi {
    config: RwLock<Option<Config>>,
    listing: RwLock<Listing>,
    refresh: Mutex<()>,
    client: Option<Arc<dyn HttpClient>>,
}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
}
fn invalid(message: &str) -> PluginError {
    PluginError::invalid(message)
}
fn valid_account(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn checked_base(value: &str) -> PluginResult<url::Url> {
    let url =
        url::Url::parse(value).map_err(|_| invalid("Enter a valid Cloudflare base address."))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid("Use a plain HTTP(S) Cloudflare base address."));
    }
    Ok(url)
}
impl CloudflareWorkersAi {
    fn config(&self) -> PluginResult<Config> {
        self.config
            .read()
            .map_err(|_| invalid("Cloudflare settings are unavailable."))?
            .clone()
            .ok_or_else(|| invalid("Initialize Cloudflare Workers AI first."))
    }
    fn client(&self, proxy: Option<&NetworkProxy>) -> PluginResult<Arc<dyn HttpClient>> {
        let config = self.config()?;
        let selected = proxy.or(config.proxy.as_ref());
        if let Some(proxy) = selected {
            proxy.validate()?;
        }
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        Ok(Arc::new(
            ReqwestHttpClient::with_client(nora_provider_plugin_sdk::http_client(selected)?)
                .with_observer_fn(|| async {
                    nora_provider_plugin_sdk::before_http_request()
                        .await
                        .map_err(|error| nora_ai::HttpClientError::Transport(error.to_string()))
                }),
        ))
    }
    fn account(&self, credentials: &Value) -> PluginResult<String> {
        let config = self.config()?;
        let value = [
            credentials.get("accountId"),
            credentials.pointer("/methods/1/key"),
            credentials.pointer("/metadata/accountId"),
            credentials.pointer("/extras/accountId"),
        ]
        .into_iter()
        .flatten()
        .find_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&config.account)
        .to_ascii_lowercase();
        if !valid_account(&value) {
            return Err(invalid(
                "Enter the 32-character Cloudflare Account ID for this account.",
            ));
        }
        Ok(value)
    }
    fn credentials(&self, value: Option<&Value>) -> PluginResult<(String, String)> {
        let value = value
            .ok_or_else(|| PluginError::authentication("Connect a Cloudflare API token first."))?;
        let key = [
            text(value, "key"),
            text(value, "apiKey"),
            text(value, "token"),
            value
                .pointer("/methods/0/key")
                .and_then(Value::as_str)
                .unwrap_or(""),
        ]
        .into_iter()
        .find(|value| !value.is_empty())
        .ok_or_else(|| PluginError::authentication("Connect a Cloudflare API token first."))?;
        Ok((self.account(value)?, key.into()))
    }
    fn base(&self, account: &str) -> PluginResult<String> {
        let value = self
            .config()?
            .template
            .replace("${CLOUDFLARE_ACCOUNT_ID}", account)
            .replace("{CLOUDFLARE_ACCOUNT_ID}", account);
        checked_base(&value)?;
        Ok(value.trim_end_matches('/').into())
    }
    fn cache_path(&self) -> PluginResult<PathBuf> {
        Ok(self.config()?.directory.join("workers-ai-models.json"))
    }
    fn saved(&self) -> Option<Listing> {
        use std::io::Read;
        let file = std::fs::File::open(self.cache_path().ok()?).ok()?;
        let mut bytes = Vec::new();
        file.take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > 8 * 1024 * 1024 {
            return None;
        }
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        if value["version"] != 1 {
            return None;
        }
        let models: Vec<Model> = serde_json::from_value(value.get("models")?.clone()).ok()?;
        if models.len() > 4096 || models.iter().any(|model| model.provider != ID) {
            return None;
        }
        Some(Listing {
            models,
            at: value["at"].as_i64(),
        })
    }
    fn save(&self, listing: &Listing) {
        let Ok(path) = self.cache_path() else { return };
        let Some(parent) = path.parent() else { return };
        if !parent.is_absolute() || std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let staged = parent.join(format!(".workers-ai-models-{}.tmp", std::process::id()));
        let result =
            serde_json::to_vec(&json!({"version":1,"at":listing.at,"models":listing.models}))
                .ok()
                .and_then(|bytes| std::fs::write(&staged, bytes).ok())
                .and_then(|_| std::fs::rename(&staged, &path).ok());
        if result.is_none() {
            let _ = std::fs::remove_file(staged);
        }
    }
    async fn models_with(&self, context: &CallContext) -> PluginResult<Value> {
        if context.credentials.is_none() {
            return Ok(
                json!({"models":self.config()?.manifest["contributes"]["provider"]["models"],"source":"preset","allowUnknownModels":true}),
            );
        }
        let (account, _) = self.credentials(context.credentials.as_ref())?;
        let client = self.client(context.network_proxy.as_ref())?;
        let _refresh = self.refresh.lock().await;
        let mut listing = self
            .listing
            .read()
            .map_err(|_| invalid("The Cloudflare model cache is unavailable."))?
            .clone();
        if listing.at.is_none() {
            if let Some(saved) = self.saved() {
                listing = saved;
            }
        }
        let now = Utc::now().timestamp_millis();
        let stale = listing.at.is_none_or(|at| {
            now.saturating_sub(at) >= self.config().map(|c| c.ttl).unwrap_or(3600000)
        });
        let mut fallback = false;
        if stale {
            match json_request(
                client.as_ref(),
                HttpRequest::new(HttpMethod::Get, CATALOG),
                64 * 1024 * 1024,
            )
            .await
            .and_then(|value| parse_models(&value))
            {
                Ok(models) => {
                    listing.models = models;
                    listing.at = Some(now);
                    self.save(&listing);
                }
                Err(error) if listing.at.is_none() => return Err(error),
                Err(_) => fallback = true,
            }
        }
        let base = self.base(&account)?;
        *self
            .listing
            .write()
            .map_err(|_| invalid("The Cloudflare model cache is unavailable."))? = listing.clone();
        let mut models = listing.models.clone();
        for model in &mut models {
            model.base_url = base.clone();
        }
        let mut value = json!({"models":models,"source":"remote","allowUnknownModels":false});
        if fallback {
            value["fellBack"] = json!(true);
            value["message"] =
                json!("The catalog could not be refreshed; showing the last fetched models.");
        }
        Ok(value)
    }
    async fn outgoing(&self, request: &FetchRequest) -> PluginResult<(HttpRequest, String)> {
        let (account, key) = self.credentials(request.credentials.as_ref())?;
        let incoming = url::Url::parse(&request.url)
            .map_err(|_| invalid("The Cloudflare request address is invalid."))?;
        if !matches!(incoming.scheme(), "http" | "https")
            || !incoming.username().is_empty()
            || incoming.password().is_some()
            || incoming.fragment().is_some()
        {
            return Err(invalid("The Cloudflare request address is invalid."));
        }
        let path = incoming.path();
        let suffix = path
            .split_once("/ai/v1")
            .map(|(_, suffix)| suffix)
            .unwrap_or(path);
        if suffix.contains("..") {
            return Err(invalid("The Cloudflare request path is invalid."));
        }
        let mut target = checked_base(&format!(
            "{}/{}",
            self.base(&account)?,
            suffix.trim_start_matches('/')
        ))?;
        target.set_query(incoming.query());
        let body = request.decode_body()?;
        let listing = self
            .listing
            .read()
            .map_err(|_| invalid("The Cloudflare model cache is unavailable."))?;
        let saved = listing.at.is_none().then(|| self.saved()).flatten();
        let known = if listing.at.is_some() {
            listing.models.as_slice()
        } else {
            saved
                .as_ref()
                .map(|value| value.models.as_slice())
                .unwrap_or_default()
        };
        if !known.is_empty() {
            if let Some(model) = body
                .as_ref()
                .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
                .and_then(|body| body.get("model").and_then(Value::as_str).map(str::to_owned))
            {
                if !known.iter().any(|known| known.id == model) {
                    return Err(invalid(
                        "This model is absent from the last known Workers AI catalog.",
                    ));
                }
            }
        }
        let mut headers = BTreeMap::from([("authorization".into(), format!("Bearer {key}"))]);
        for (name, value) in &request.headers {
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "content-type" | "cf-aig-authorization" | "cf-aig-metadata"
            ) {
                headers.insert(name.to_ascii_lowercase(), value.clone());
            }
        }
        let method = match request.method.as_str() {
            "POST" => HttpMethod::Post,
            "GET" => HttpMethod::Get,
            "PUT" => HttpMethod::Put,
            "PATCH" => HttpMethod::Patch,
            "DELETE" => HttpMethod::Delete,
            _ => return Err(invalid("This Cloudflare request method is unsupported.")),
        };
        Ok((
            HttpRequest {
                method,
                url: target.to_string(),
                headers,
                body,
            },
            key,
        ))
    }
}
fn parse_models(value: &Value) -> PluginResult<Vec<Model>> {
    if !value
        .pointer("/cloudflare-workers-ai/models")
        .is_some_and(Value::is_object)
    {
        return Err(PluginError::transport(
            "The Workers AI section is absent from the model catalog.",
        ));
    }
    let catalog =
        nora_provider::models::models_dev_catalog(value).map_err(PluginError::transport)?;
    let entries = &value[ID]["models"];
    let mut models = catalog
        .models_for(ID)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    if models.len() > 4096 {
        return Err(PluginError::transport(
            "The Workers AI catalog is too large.",
        ));
    }
    for model in &mut models {
        let entry = &entries[&model.id];
        model.api = "openai-completions".into();
        model.provider = ID.into();
        model.base_url = DEFAULT_BASE.into();
        model.info.free = Some(false);
        if entry.pointer("/limit/context").is_none() {
            model.context_window = 32768;
        }
        if entry.pointer("/limit/output").is_none() {
            model.max_tokens = 4096;
        }
        if PAID.contains(&model.id.as_str()) {
            model.name = format!("{} (Paid)", model.name);
        }
    }
    Ok(models)
}
async fn json_request(
    client: &dyn HttpClient,
    request: HttpRequest,
    max: usize,
) -> PluginResult<Value> {
    tokio::time::timeout(Duration::from_secs(15), async {
        let response = client
            .stream(request)
            .await
            .map_err(|_| PluginError::transport("The Cloudflare service could not be reached."))?;
        if response.status != 200 {
            return Err(PluginError::transport(format!(
                "The Cloudflare service returned HTTP {}.",
                response.status
            )));
        }
        let body = nora_ai::collect_http_body_text_limited(response.body, max)
            .await
            .map_err(|_| {
                PluginError::transport("The Cloudflare response is incomplete or too large.")
            })?;
        serde_json::from_str(&body)
            .map_err(|_| PluginError::transport("The Cloudflare response is invalid."))
    })
    .await
    .map_err(|_| PluginError::transport("The Cloudflare request timed out."))?
}
#[async_trait]
impl ProviderPlugin for CloudflareWorkersAi {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        if params.api_version != API_VERSION
            || params.plugin_id != ID
            || params.manifest["id"] != ID
        {
            return Err(invalid("The Cloudflare provider identity is invalid."));
        }
        let config = &params.manifest["contributes"]["provider"]["config"];
        let template = config
            .get("baseTemplate")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_BASE)
            .to_owned();
        checked_base(&template)?;
        let proxy = params
            .manifest
            .pointer("/contributes/provider/config/networkProxy")
            .cloned()
            .filter(|value| !value.is_null())
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| invalid("The Cloudflare proxy configuration is invalid."))?;
        let account = text(config, "accountId").to_owned();
        let ttl = config["discoveryTTLMs"].as_i64().unwrap_or(3600000).max(0);
        *self
            .config
            .write()
            .map_err(|_| invalid("Cloudflare settings are unavailable."))? = Some(Config {
            manifest: params.manifest,
            proxy,
            directory: params.data_directory,
            template,
            account,
            ttl,
        });
        Ok(json!({"apiVersion":API_VERSION,"capabilities":{"persistent":true,"maxConcurrency":3}}))
    }
    async fn auth_methods(&self, _context: CallContext) -> PluginResult<Value> {
        Ok(self.config()?.manifest["contributes"]["provider"]["authMethods"].clone())
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        let method = params
            .get("authMethodID")
            .or_else(|| params.get("method"))
            .and_then(Value::as_str)
            .unwrap_or("apiKey");
        if method != "apiKey" {
            return Err(PluginError::unsupported(method));
        }
        let input = params
            .get("inputs")
            .or_else(|| params.get("fields"))
            .unwrap_or(&params);
        let key = [
            text(input, "token"),
            text(input, "key"),
            text(input, "apiKey"),
        ]
        .into_iter()
        .find(|value| !value.is_empty())
        .ok_or_else(|| invalid("Enter the Cloudflare API token."))?;
        if key.len() > 16384 || key.chars().any(char::is_control) {
            return Err(invalid("The Cloudflare API token is invalid."));
        }
        let account = self.account(input)?;
        Ok(
            json!({"ok":true,"credentials":{"type":"api","authMethod":"apiKey","key":key,"accountId":account,"name":account,"accountIdentity":{"issuer":"https://api.cloudflare.com","subject":account}}}),
        )
    }
    async fn accounts(&self, _context: CallContext) -> PluginResult<Value> {
        self.config()?;
        Ok(json!([]))
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        self.models_with(&context).await
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let fallback = "Workers Free / Paid (confirm in the dashboard)";
        let Ok((account, key)) = self.credentials(context.credentials.as_ref()) else {
            return Ok(nora_provider::usage::unavailable(
                "Connect a Cloudflare API token and Account ID to view its allowance.",
            ));
        };
        let client = self.client(context.network_proxy.as_ref())?;
        let info = json_request(
            client.as_ref(),
            HttpRequest::new(
                HttpMethod::Get,
                format!("https://api.cloudflare.com/client/v4/accounts/{account}"),
            )
            .with_header("authorization", format!("Bearer {key}")),
            1024 * 1024,
        )
        .await
        .ok();
        let name = info
            .as_ref()
            .and_then(|info| {
                info.pointer("/result/plan/name")
                    .and_then(Value::as_str)
                    .or_else(|| info.pointer("/result/plan").and_then(Value::as_str))
            })
            .filter(|name| !name.is_empty());
        let plan = name
            .map(|name| {
                if name.to_ascii_lowercase().contains("free") {
                    "Workers Free"
                } else {
                    name
                }
            })
            .unwrap_or(fallback);
        let now = Utc::now().timestamp();
        let reset = Utc
            .timestamp_opt(now.div_euclid(86400) * 86400 + 86400, 0)
            .single()
            .unwrap();
        Ok(nora_provider::usage::parsing::result(
            &json!({"plan_type":plan}),
            vec![
                json!({"id":"neurons-day","label":"Neurons/day (free allowance)","limit":10000,"unit":"neurons","windowSeconds":86400,"resetsAt":reset.to_rfc3339(),"countsForRouting":false,"value":"Daily free allocation: 10,000 Neurons; exact usage is shown in the Cloudflare dashboard."}),
            ],
            Vec::new(),
        ))
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        let client = self.client(request.network_proxy.as_ref())?;
        let (outgoing, key) = self.outgoing(&request).await?;
        let response = client
            .stream(outgoing)
            .await
            .map_err(|_| PluginError::transport("The Cloudflare endpoint could not be reached."))?;
        if response.status == 401 {
            return Err(PluginError::authentication(
                "The Cloudflare API token was refused. Connect this account again.",
            ));
        }
        nora_provider::transport::finish_response(response, vec![key]).await
    }
    async fn call(&self, method: &str, _params: Value) -> PluginResult<Value> {
        self.config()?;
        if method == "maintain" {
            return Ok(json!({"ok":true,"modelsChanged":false}));
        }
        Err(PluginError::unsupported(method))
    }
}
#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
