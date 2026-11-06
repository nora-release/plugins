use crate::{
    browser::BrowserService,
    cli::Gcloud,
    credentials,
    models::{self, ModelLists},
    settings::Settings,
};
use async_trait::async_trait;
use nora_ai::{HttpClient, HttpMethod, HttpRequest, ReqwestHttpClient};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, PluginError,
    PluginResult, ProviderPlugin, API_VERSION,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

#[derive(Clone)]
struct Config {
    row: Value,
    directory: PathBuf,
    proxy: Option<NetworkProxy>,
    methods: Value,
    preset: Value,
}
struct Pending {
    started: Instant,
    task: tokio::task::JoinHandle<PluginResult<Value>>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Default)]
pub(crate) struct GcpAi {
    config: RwLock<Option<Config>>,
    cli: Gcloud,
    browser: Arc<BrowserService>,
    lists: ModelLists,
    client: Option<Arc<dyn HttpClient>>,
    pending: Mutex<BTreeMap<String, Pending>>,
    endorsed: Mutex<BTreeSet<String>>,
}
impl GcpAi {
    fn config(&self) -> PluginResult<Config> {
        self.config
            .read()
            .map_err(|_| PluginError::invalid("The GCP AI configuration is unavailable."))?
            .clone()
            .ok_or_else(|| PluginError::invalid("Initialize GCP AI before using it."))
    }
    fn settings(&self, auth: Option<&Value>) -> PluginResult<Settings> {
        let config = self.config()?;
        Settings::resolve(&config.row, auth, &config.directory)
    }
    fn client(&self, proxy: Option<&NetworkProxy>) -> PluginResult<Arc<dyn HttpClient>> {
        let config = self.config()?;
        let proxy = proxy.or(config.proxy.as_ref());
        if let Some(proxy) = proxy {
            proxy.validate()?;
        }
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        Ok(Arc::new(
            ReqwestHttpClient::with_client(nora_provider_plugin_sdk::http_client(proxy)?)
                .with_observer_fn(|| async {
                    nora_provider_plugin_sdk::before_http_request()
                        .await
                        .map_err(|error| nora_ai::HttpClientError::Transport(error.to_string()))
                }),
        ))
    }
    async fn identity(
        &self,
        credentials: &Value,
        client: &dyn HttpClient,
    ) -> PluginResult<Settings> {
        if credentials.get("type").and_then(Value::as_str) != Some("oauth")
            || credentials
                .pointer("/extras/source")
                .and_then(Value::as_str)
                != Some("gcloud")
        {
            return Err(PluginError::authentication(
                "Connect a Google Cloud ADC or gcloud account.",
            ));
        }
        let mut identity =
            credentials::resolve(&self.cli, &self.settings(Some(credentials))?).await?;
        if identity.account.is_none() && identity.auth_mode == "adc" {
            identity.account = credentials::owner(&self.cli, &identity, client).await;
            if let Some(account) = &identity.account {
                let mut updated = credentials.clone();
                updated["extras"]["accountId"] = json!(account);
                updated["extras"]["email"] = json!(account);
                updated["accountIdentity"] = json!({"subject":account.to_lowercase()});
                let _ = nora_provider_plugin_sdk::save_credentials(updated).await;
            }
        }
        Ok(identity)
    }
    async fn endorse(
        &self,
        identity: &Settings,
        token: &str,
        client: &dyn HttpClient,
    ) -> PluginResult<()> {
        let key = crate::cache::digest(&(
            &identity.account,
            &identity.cloudsdk_config,
            &identity.project,
            &identity.location,
        ));
        let mut endorsed = self.endorsed.lock().await;
        if endorsed.contains(&key) {
            return Ok(());
        }
        let project = identity.project.as_deref().unwrap();
        let request = credentials::authorized(
            HttpMethod::Get,
            format!(
                "https://aiplatform.googleapis.com/v1/projects/{project}/locations/{}",
                identity.location
            ),
            token,
            Some(project),
        );
        let response = tokio::time::timeout(Duration::from_secs(15), client.stream(request))
            .await
            .map_err(|_| {
                PluginError::transport("The Google model endpoint did not answer in time.")
            })?
            .map_err(|_| {
                PluginError::transport("The Google model endpoint could not be reached.")
            })?;
        let status = response.status;
        if status == 401 {
            return Err(PluginError::authentication(
                "Google Cloud rejected this access token. Refresh the account and try again.",
            ));
        }
        if matches!(status, 429 | 500 | 502 | 503 | 504) {
            return Err(PluginError::transport(format!("The Google model endpoint is temporarily unavailable (HTTP {status}). Credentials were kept; try again.")));
        }
        if (200..300).contains(&status) {
            let mut request=credentials::authorized(HttpMethod::Post,format!("https://aiplatform.googleapis.com/v1/projects/{project}/locations/{}/publishers/google/models/gemini-2.5-flash:countTokens",identity.location),token,Some(project)).with_header("content-type","application/json");
            request.body =
                Some(br#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#.to_vec());
            let _ = tokio::time::timeout(Duration::from_secs(15), client.stream(request)).await;
        }
        endorsed.insert(key);
        Ok(())
    }
}

fn inference(request: FetchRequest, identity: &Settings, token: &str) -> PluginResult<HttpRequest> {
    let project = identity
        .project
        .as_deref()
        .ok_or_else(|| PluginError::invalid("Choose a Google Cloud project."))?;
    let base = url::Url::parse(&identity.endpoint(project)?).unwrap();
    let target = url::Url::parse(&request.url)
        .map_err(|_| PluginError::invalid("The GCP AI request address is invalid."))?;
    let suffix = target
        .path()
        .strip_prefix(base.path())
        .filter(|suffix| suffix.starts_with('/'))
        .ok_or_else(|| {
            PluginError::invalid("The request is outside this Google account's model endpoint.")
        })?;
    if target.origin() != base.origin()
        || !target.username().is_empty()
        || target.password().is_some()
        || target.fragment().is_some()
        || !gemini_route(suffix)
    {
        return Err(PluginError::invalid(
            "A Vertex credential was refused outside the configured Google model endpoint.",
        ));
    }
    let method = match request.method.to_ascii_uppercase().as_str() {
        "GET" => HttpMethod::Get,
        "POST" => HttpMethod::Post,
        _ => {
            return Err(PluginError::invalid(
                "The Google model endpoint accepts GET or POST.",
            ))
        }
    };
    let body = request.decode_body()?;
    let mut headers = request
        .headers
        .into_iter()
        .filter(|(name, _)| {
            !matches!(
                name.to_ascii_lowercase().as_str(),
                "authorization"
                    | "x-goog-api-key"
                    | "x-api-key"
                    | "api-key"
                    | "cookie"
                    | "proxy-authorization"
                    | "x-goog-user-project"
                    | "host"
                    | "content-length"
                    | "connection"
                    | "transfer-encoding"
            )
        })
        .collect::<BTreeMap<_, _>>();
    headers.insert("authorization".into(), format!("Bearer {token}"));
    headers.insert("x-goog-user-project".into(), project.into());
    Ok(HttpRequest {
        method,
        url: target.into(),
        headers,
        body,
    })
}
fn gemini_route(path: &str) -> bool {
    if path == "/models" {
        return true;
    }
    let Some(model) = path.strip_prefix("/models/") else {
        return false;
    };
    if model.is_empty() || model.contains('/') {
        return false;
    }
    if let Some((id, operation)) = model.split_once(':') {
        return !id.is_empty()
            && matches!(
                operation,
                "generateContent" | "streamGenerateContent" | "countTokens"
            );
    }
    true
}
fn usage(credits: Value, identity: &Settings) -> Value {
    let remaining = credits["remaining"].as_f64().unwrap_or_default();
    let original = credits["original"].as_f64().unwrap_or_default();
    let mut item = json!({"id":"promotional_credits","label":"Promotional Credits","usedPercent":credits["usedPercent"],"amount":credits["used"],"limit":original,"unit":"USD","countsForRouting":true,"description":format!("${remaining:.2} remaining / ${original:.2} USD{}",if credits["cached"]==true{" (cached)"}else{""})});
    for key in ["runsOutAt", "runsOut"] {
        if let Some(value) = credits.get(key) {
            item[key] = value.clone();
        }
    }
    json!({"available":true,"plan":"Google Cloud promotional credits","balance":format!("${remaining:.2} USD"),"signIn":"kept","items":[item],"account":{"email":identity.account},"fetchedAt":credits["fetchedAt"]})
}

#[async_trait]
impl ProviderPlugin for GcpAi {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        if params.api_version != API_VERSION
            || params.plugin_id != "gcp-ai"
            || params.manifest["id"] != "gcp-ai"
        {
            return Err(PluginError::invalid(
                "The GCP AI provider manifest is invalid.",
            ));
        }
        let row = params
            .manifest
            .pointer("/contributes/provider/config")
            .cloned()
            .unwrap_or(json!({}));
        let proxy = serde_json::from_value::<Option<NetworkProxy>>(
            row.get("networkProxy").cloned().unwrap_or(Value::Null),
        )
        .map_err(|_| PluginError::invalid("The network proxy configuration is invalid."))?;
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        *self
            .config
            .write()
            .map_err(|_| PluginError::invalid("The GCP AI configuration is unavailable."))? =
            Some(Config {
                row,
                directory: params.data_directory,
                proxy,
                methods: params
                    .manifest
                    .pointer("/contributes/provider/authMethods")
                    .cloned()
                    .unwrap_or(json!([])),
                preset: params
                    .manifest
                    .pointer("/contributes/provider/models")
                    .cloned()
                    .unwrap_or_else(models::seeds),
            });
        Ok(json!({"apiVersion":API_VERSION}))
    }
    async fn auth_methods(&self, _context: CallContext) -> PluginResult<Value> {
        Ok(self.config()?.methods)
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        nora_provider_plugin_sdk::require_subscription_accounts()?;
        let method = params
            .get("authMethodID")
            .or_else(|| params.get("method"))
            .and_then(Value::as_str)
            .unwrap_or("gcloud");
        if method != "gcloud" {
            return Err(PluginError::unsupported(method));
        }
        let config = self.config()?;
        let inputs = params.get("inputs").or_else(|| params.get("fields"));
        let settings = Settings::resolve(&config.row, inputs, &config.directory)?;
        let proxy = serde_json::from_value::<Option<NetworkProxy>>(
            params.get("networkProxy").cloned().unwrap_or(Value::Null),
        )
        .map_err(|_| PluginError::invalid("The network proxy configuration is invalid."))?;
        let client = self.client(proxy.as_ref())?;
        let cli = self.cli.clone();
        let browser = self.browser.clone();
        let mut pending = self.pending.lock().await;
        pending.retain(|_, pending| pending.started.elapsed() < Duration::from_secs(600));
        if pending.len() >= 16 {
            return Err(PluginError::invalid(
                "Too many Google sign-ins are pending. Finish one before trying again.",
            ));
        }
        let session = uuid::Uuid::new_v4().to_string();
        let task = tokio::spawn(async move {
            tokio::time::timeout(
                Duration::from_secs(600),
                credentials::start_login(settings, &cli, client.as_ref(), browser.as_ref()),
            )
            .await
            .map_err(|_| {
                PluginError::authentication("Google sign-in did not finish within ten minutes.")
            })?
        });
        pending.insert(
            session.clone(),
            Pending {
                started: Instant::now(),
                task,
            },
        );
        Ok(
            json!({"session":session,"url":"","method":"auto","instructions":"A Chrome window opens for Google consent. Approve the intended Google account there and leave the window open; Nora closes it when sign-in finishes."}),
        )
    }
    async fn callback(&self, params: Value) -> PluginResult<Value> {
        let session = params
            .get("session")
            .and_then(Value::as_str)
            .ok_or_else(|| PluginError::invalid("Choose the pending Google sign-in."))?;
        let mut pending = self.pending.lock().await.remove(session).ok_or_else(|| {
            PluginError::authentication("This Google sign-in expired. Start again.")
        })?;
        (&mut pending.task)
            .await
            .map_err(|_| PluginError::authentication("Google sign-in was stopped."))?
    }
    async fn accounts(&self, context: CallContext) -> PluginResult<Value> {
        let Some(credentials) = context.credentials.as_ref() else {
            return Ok(json!([]));
        };
        let client = self.client(context.network_proxy.as_ref())?;
        let identity = self.identity(credentials, client.as_ref()).await?;
        Ok(
            json!([{"id":context.account,"email":identity.account,"name":identity.account.as_deref().unwrap_or("Signed in"),"plan":"Google Cloud"}]),
        )
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let credentials = params
            .get("credentials")
            .ok_or_else(|| PluginError::authentication("Connect a Google Cloud account."))?;
        let identity = credentials::resolve(&self.cli, &self.settings(Some(credentials))?).await?;
        let _ = self.cli.token(&identity).await?;
        Ok(json!({"changed":false}))
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        if context.credentials.is_none() {
            return Ok(
                json!({"models":self.config()?.preset,"source":"preset","allowUnknownModels":false,"defaultAPI":"google-generative-ai","defaultBaseURL":"https://aiplatform.googleapis.com"}),
            );
        }
        let settings = self.settings(context.credentials.as_ref())?;
        let client = self.client(context.network_proxy.as_ref())?;
        let available = self
            .lists
            .available(settings.clone(), self.cli.clone(), client)
            .await?;
        let mut identity = settings;
        identity.project = available
            .get("project")
            .and_then(Value::as_str)
            .map(str::to_owned);
        identity.location = available["location"]
            .as_str()
            .unwrap_or(&identity.location)
            .to_owned();
        let ids = available["ids"]
            .as_array()
            .ok_or_else(|| PluginError::transport("The Google model list is invalid."))?
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        models::model_rows(&ids, &identity, &models::catalog(&identity))
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let result = async {
            let credentials = context
                .credentials
                .as_ref()
                .ok_or_else(|| PluginError::authentication("GCP AI is not signed in."))?;
            let client = self.client(context.network_proxy.as_ref())?;
            let identity = self.identity(credentials, client.as_ref()).await?;
            let credits = crate::credits::read(self.browser.as_ref(), &identity).await?;
            Ok::<_, PluginError>(usage(credits, &identity))
        }
        .await;
        match result {
            Ok(usage) => Ok(usage),
            Err(error) => Ok(json!({"available":false,"error":error.to_string(),"signIn":"kept"})),
        }
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        if !request.subscription_accounts_enabled {
            return Err(PluginError::authentication(
                "Subscription accounts are disabled.",
            ));
        }
        let credentials = request
            .credentials
            .as_ref()
            .ok_or_else(|| PluginError::authentication("Connect a Google Cloud account."))?;
        let client = self.client(request.network_proxy.as_ref())?;
        let identity = self.identity(credentials, client.as_ref()).await?;
        // Validate the destination before even asking gcloud to produce a token.
        inference(request.clone(), &identity, "")?;
        let token = self.cli.token(&identity).await?;
        self.cli
            .verify_owner(&identity, &token, client.as_ref())
            .await?;
        self.endorse(&identity, &token, client.as_ref()).await?;
        let request = inference(request, &identity, &token)?;
        let secrets = nora_provider::transport::request_secrets(&request);
        let response = client.stream(request).await.map_err(|_| {
            PluginError::transport("The Google model endpoint could not be reached.")
        })?;
        nora_provider::transport::finish_response(response, secrets).await
    }
}

#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
