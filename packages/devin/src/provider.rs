use super::{
    account::{self, Account},
    auth,
    models::{self, Model},
    network::{self, Client},
    protobuf, request, stream, usage,
};
use async_trait::async_trait;
use nora_ai::{HttpMethod, HttpRequest, ReqwestHttpClient};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, PluginError,
    PluginResult, ProviderPlugin,
};
use nora_provider_subscription_core::{self as core, Core};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
pub(crate) struct Devin {
    core: Core,
    client: Option<Client>,
    pending: Mutex<BTreeMap<String, (Instant, auth::Pending)>>,
    models: Mutex<BTreeMap<String, (Instant, Vec<Model>)>>,
    persistence: Arc<dyn nora_provider::account_import::CredentialPersistence>,
}
impl Default for Devin {
    fn default() -> Self {
        Self {
            core: Core::new("devin"),
            client: None,
            pending: Mutex::new(BTreeMap::new()),
            models: Mutex::new(BTreeMap::new()),
            persistence: Arc::new(nora_provider::account_import::HostPersistence),
        }
    }
}
impl Devin {
    async fn save_profile(&self, account: &Account) {
        let credentials = account.credentials();
        if credentials != account.saved {
            // Profile enrichment is best effort: a storage failure must not
            // hide the account information Devin has successfully returned.
            let _ = self.persistence.save(credentials).await;
        }
    }
    fn client(&self, proxy: Option<&NetworkProxy>) -> PluginResult<Client> {
        let config = self.core.config()?;
        let proxy = proxy.or(config.proxy.as_ref());
        if let Some(proxy) = proxy {
            proxy.validate()?;
        }
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        let client = nora_provider_plugin_sdk::http_client_builder(proxy)?
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| PluginError::invalid("The Devin HTTP client could not be configured."))?;
        Ok(Arc::new(
            ReqwestHttpClient::with_client(client).with_observer_fn(|| async {
                nora_provider_plugin_sdk::before_http_request()
                    .await
                    .map_err(|error| nora_ai::HttpClientError::Transport(error.to_string()))
            }),
        ))
    }
    fn params_client(&self, params: &Value) -> PluginResult<Client> {
        let proxy: Option<NetworkProxy> =
            serde_json::from_value(params.get("networkProxy").cloned().unwrap_or(Value::Null))
                .map_err(|_| PluginError::invalid("The Devin proxy settings are invalid."))?;
        self.client(proxy.as_ref())
    }
    async fn listing(
        &self,
        client: &dyn nora_ai::HttpClient,
        account: &Account,
        fresh: bool,
    ) -> PluginResult<Vec<Model>> {
        let key = core::fingerprint(&json!([account.key, account.server]));
        if !fresh {
            if let Some((_, rows)) = self
                .models
                .lock()
                .await
                .get(&key)
                .filter(|(at, _)| at.elapsed() < Duration::from_secs(300))
            {
                return Ok(rows.clone());
            }
        }
        let rows = models::fetch(client, &account.key, &account.server).await?;
        let mut cache = self.models.lock().await;
        cache.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(600));
        if cache.len() >= 64 {
            cache.clear();
        }
        cache.insert(key, (Instant::now(), rows.clone()));
        Ok(rows)
    }
}
#[async_trait]
impl ProviderPlugin for Devin {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        if params.manifest["baseURL"] != network::BASE
            || params.manifest["api"] != "openai-completions"
        {
            return Err(PluginError::invalid(
                "Devin requires its declared native endpoint.",
            ));
        }
        self.core.initialize(params)
    }
    async fn auth_methods(&self, _: CallContext) -> PluginResult<Value> {
        Ok(self.core.config()?.manifest["authMethods"].clone())
    }
    async fn accounts(&self, context: CallContext) -> PluginResult<Value> {
        let Some(value) = context.credentials else {
            return Ok(json!([]));
        };
        let account = if value.pointer("/metadata/profileVersion") == Some(&json!(1)) {
            Account::parse(&value)?
        } else {
            let client = self.client(context.network_proxy.as_ref())?;
            let (account, _) = Account::status(&value, client.as_ref()).await?;
            self.save_profile(&account).await;
            account
        };
        Ok(json!([account.profile.public(&context.account)]))
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        let client = self.params_client(&params)?;
        let method = params
            .get("authMethodID")
            .or_else(|| params.get("method"))
            .and_then(Value::as_str)
            .unwrap_or("browser");
        match method {
            "browser" => {
                let (pending, url) = auth::start(client).await?;
                let session = uuid::Uuid::new_v4().to_string();
                let mut sessions = self.pending.lock().await;
                sessions.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(600));
                if sessions.len() >= 8 {
                    return Err(PluginError::invalid(
                        "Finish an existing Devin sign-in first.",
                    ));
                }
                sessions.insert(session.clone(), (Instant::now(), pending));
                Ok(
                    json!({"session":session,"url":url,"method":"auto","instructions":"Sign in to Devin in your browser."}),
                )
            }
            "cliSource" => {
                let path = core::required(core::inputs(&params), "sourcePath")?.to_owned();
                let selected = path.clone();
                let (key, server) = tokio::task::spawn_blocking(move || account::read(&selected))
                    .await
                    .map_err(|_| account::expired())??;
                let account = auth::finish(client.as_ref(), key, server, Some(path)).await?;
                Ok(json!({"ok":true,"credentials":account.credentials()}))
            }
            _ => Err(PluginError::unsupported(method)),
        }
    }
    async fn callback(&self, params: Value) -> PluginResult<Value> {
        let session = core::required(&params, "session")?;
        let (_, pending) = self
            .pending
            .lock()
            .await
            .remove(session)
            .ok_or_else(account::expired)?;
        let account = pending.complete().await?;
        Ok(json!({"ok":true,"credentials":account.credentials()}))
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let client = self.params_client(&params)?;
        let (account, _) = Account::status(
            params.get("credentials").ok_or_else(account::expired)?,
            client.as_ref(),
        )
        .await?;
        let value = account.credentials();
        Ok(json!({"changed":value!=account.saved,"credentials":value}))
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        self.core.config()?;
        let Some(value) = context.credentials else {
            return Ok(
                json!({"models":[],"source":"preset","allowUnknownModels":false,"defaultAPI":"openai-completions","defaultBaseURL":network::BASE}),
            );
        };
        let client = self.client(context.network_proxy.as_ref())?;
        let account = Account::live(&value, client.as_ref()).await?;
        Ok(models::output(
            &self.listing(client.as_ref(), &account, true).await?,
        ))
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let client = self.client(context.network_proxy.as_ref())?;
        let (account, value) = Account::usage_status(
            context.credentials.as_ref().ok_or_else(account::expired)?,
            client.as_ref(),
        )
        .await?;
        self.save_profile(&account).await;
        let rows = self.listing(client.as_ref(), &account, false).await?;
        usage::parse(&value, &account, &context.account, &rows)
    }
    async fn fetch(&self, original: FetchRequest) -> PluginResult<FetchResponse> {
        let chat = request::parse(&original)?;
        let client = self.client(original.network_proxy.as_ref())?;
        let account = Account::live(
            original.credentials.as_ref().ok_or_else(account::expired)?,
            client.as_ref(),
        )
        .await?;
        let rows = self.listing(client.as_ref(), &account, false).await?;
        let effort = chat["reasoning_effort"]
            .as_str()
            .or_else(|| original.metadata.get("reasoningEffort").map(String::as_str))
            .unwrap_or_default();
        let uid = models::variant(&rows, chat["model"].as_str().unwrap(), effort)?;
        let model = rows.iter().find(|row| row.id == uid).ok_or_else(|| {
            PluginError::invalid("This Devin account does not offer the selected model.")
        })?;
        if !model.image
            && chat["messages"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|m| m["content"].as_array().into_iter().flatten())
                .any(|p| p["type"] == "image_url")
        {
            return Ok(stream::failure(415));
        }
        let body = match request::build(&chat, &uid, &account.key) {
            Ok(body) => body,
            Err(error) if error.code == "unsupported_media" => return Ok(stream::failure(415)),
            Err(error) => return Err(error),
        };
        let frame = protobuf::frame(&body)?;
        let compressed = frame[0] & 1 != 0;
        let mut request = HttpRequest::new(
            HttpMethod::Post,
            format!("{}{}", account.server, network::CHAT),
        )
        .with_header("content-type", "application/connect+proto")
        .with_header("connect-protocol-version", "1")
        .with_header("authorization", format!("Basic {}", account.key))
        .with_body(frame);
        if compressed {
            request
                .headers
                .insert("connect-content-encoding".into(), "gzip".into());
        }
        stream::response(network::send(client.as_ref(), request).await?, &chat).await
    }
    async fn call(&self, method: &str, _: Value) -> PluginResult<Value> {
        if method == "maintain" {
            return Ok(json!({"ok":true,"modelsChanged":false}));
        }
        Err(PluginError::unsupported(method))
    }
}
#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;

#[cfg(test)]
#[path = "../test/organization.test.rs"]
mod organization_tests;
