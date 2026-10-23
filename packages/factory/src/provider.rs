use super::{
    account::{self, Account},
    attachments, auth, models,
    network::{self, Client},
    request, stream, usage,
};
use async_trait::async_trait;
use nora_ai::{HttpMethod, HttpRequest, ReqwestHttpClient};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, PluginError,
    PluginResult, ProviderPlugin,
};
use nora_provider_subscription_core::{self as core, Core};
use serde_json::{json, Value};
use std::sync::Arc;
pub(crate) struct Factory {
    pub core: Core,
    client: Option<Client>,
    session: String,
}
impl Default for Factory {
    fn default() -> Self {
        Self {
            core: Core::new("factory"),
            client: None,
            session: uuid::Uuid::new_v4().to_string(),
        }
    }
}
impl Factory {
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
            .map_err(|_| {
                PluginError::invalid("The Factory network client could not be configured.")
            })?;
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
                .map_err(|_| PluginError::invalid("The Factory proxy is invalid."))?;
        self.client(proxy.as_ref())
    }
    async fn rotate(&self, old: &Account, next: &mut Account) -> PluginResult<()> {
        let value = next.credentials();
        self.core
            .persist_rotation(&old.saved, value.clone())
            .await?;
        next.saved = value;
        Ok(())
    }
    async fn live(
        &self,
        client: &dyn nora_ai::HttpClient,
        value: Option<&Value>,
        force: bool,
    ) -> PluginResult<Account> {
        let _lock = self.core.refresh_lock.lock().await;
        let value = value.ok_or_else(account::expired)?;
        let latest = self.core.latest(value).await?;
        let mut account = Account::parse(&latest)?;
        if account.key {
            if account.identity().is_empty() {
                let old = account.clone();
                network::reconcile(client, &mut account).await?;
                if account.identity().is_empty() {
                    return Err(account::expired());
                }
                self.rotate(&old, &mut account).await?;
            }
            return Ok(account);
        }
        let lead = if force { 180000 } else { 120000 };
        let stale = account.expires > 0 && core::now() >= account.expires.saturating_sub(lead);
        if force && latest != *value && account.expires > core::now().saturating_add(180000) {
            return Ok(account);
        }
        if force || stale {
            let old = account.clone();
            match auth::renew(client, &account, "").await {
                Ok(next) => account = next,
                Err(error) => {
                    if error.code.starts_with("factory_http_4") && error.code != "factory_http_429"
                    {
                        return Err(account::expired());
                    }
                    if force || account.expires == 0 || core::now() >= account.expires {
                        return Err(error);
                    }
                    return Ok(account);
                }
            }
            self.rotate(&old, &mut account).await?;
        }
        Ok(account)
    }
    async fn mend(
        &self,
        client: &dyn nora_ai::HttpClient,
        account: &mut Account,
        value: &Value,
    ) -> PluginResult<bool> {
        if account.key {
            return Ok(false);
        }
        let _lock = self.core.refresh_lock.lock().await;
        let latest = self.core.latest(&account.saved).await?;
        let mut next = Account::parse(&latest)?;
        let old = next.clone();
        let refused = network::org_refused(403, value);
        if !next.active_org.is_empty() && refused {
            let denied = next.active_org.clone();
            next.active_org.clear();
            network::reconcile(client, &mut next).await?;
            if next.active_org == denied {
                next.active_org.clear();
            }
        } else if next.active_org.is_empty() && refused {
            let org = auth::first_org(client, &next).await?;
            if org.is_empty() {
                return Ok(false);
            }
            next = auth::renew(client, &next, &org).await?;
        } else if next.active_org.is_empty() {
            network::reconcile(client, &mut next).await?;
            if next.active_org.is_empty() {
                return Ok(false);
            }
        } else {
            return Ok(false);
        }
        if next.credentials() == old.credentials() {
            return Ok(false);
        }
        self.rotate(&old, &mut next).await?;
        *account = next;
        Ok(true)
    }
    fn outgoing(
        &self,
        parsed: &request::Request,
        original: &FetchRequest,
        account: &Account,
    ) -> PluginResult<HttpRequest> {
        let mut outgoing = network::factory(
            HttpMethod::Post,
            format!("{}{}{}", account.llm_base()?, parsed.path, parsed.query),
            account,
        )
        .with_header("content-type", "application/json")
        .with_header("x-api-provider", parsed.model["upstream"].as_str().unwrap())
        .with_header("x-assistant-message-id", uuid::Uuid::new_v4().to_string())
        .with_header("x-provider-routing-source", "registry_default");
        let session = original
            .metadata
            .get("sessionID")
            .or_else(|| original.metadata.get("sessionId"))
            .map(|session| {
                let hex = core::fingerprint(&json!([account.identity(), session]));
                format!(
                    "{}-{}-4{}-8{}-{}",
                    &hex[..8],
                    &hex[8..12],
                    &hex[13..16],
                    &hex[17..20],
                    &hex[20..32]
                )
            })
            .unwrap_or_else(|| self.session.clone());
        outgoing.headers.insert("x-session-id".into(), session);
        if parsed.model["upstream"] == "openai" {
            outgoing.headers.insert(
                "openai-platform".into(),
                "org-bHuLtG1fGmYk5YaOihAAXFBw".into(),
            );
        }
        if parsed.wire == models::Wire::Anthropic {
            outgoing
                .headers
                .insert("x-api-key".into(), "placeholder".into());
            for (k, v) in &original.headers {
                if k.eq_ignore_ascii_case("anthropic-version") && !v.contains(['\r', '\n']) {
                    outgoing
                        .headers
                        .insert("anthropic-version".into(), v.clone());
                }
            }
            let mut beta = original
                .headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("anthropic-beta"))
                .map(|(_, v)| {
                    v.split(',')
                        .map(str::trim)
                        .filter(|s| {
                            !s.is_empty()
                                && s.len() <= 128
                                && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                        })
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if parsed.fast && !beta.iter().any(|s| s == request::FAST_BETA) {
                beta.push(request::FAST_BETA.into());
            }
            if !beta.is_empty() {
                outgoing
                    .headers
                    .insert("anthropic-beta".into(), beta.join(","));
            }
        }
        outgoing.body = Some(
            serde_json::to_vec(&parsed.body)
                .map_err(|_| PluginError::invalid("The Factory request could not be encoded."))?,
        );
        Ok(outgoing)
    }
}
#[async_trait]
impl ProviderPlugin for Factory {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        if params.manifest["baseURL"] != network::BASE
            || params.manifest["api"] != "anthropic-messages"
        {
            return Err(PluginError::invalid(
                "Factory requires its declared native endpoint.",
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
        let account = Account::parse(&value)?;
        Ok(
            json!([{"id":context.account,"name":if account.email.is_empty(){&account.user}else{&account.email}}]),
        )
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        let client = self.params_client(&params)?;
        let method = params
            .get("authMethodID")
            .or_else(|| params.get("method"))
            .and_then(Value::as_str)
            .unwrap_or("device");
        match method {
            "apiKey" => {
                let key = core::required(core::inputs(&params), "apiKey")?;
                if !key.starts_with("fk-") || key.chars().any(char::is_whitespace) {
                    return Err(PluginError::invalid(
                        "Enter a Factory API key beginning with fk-.",
                    ));
                }
                let mut account = Account::parse(&json!({"type":"api","key":key}))?;
                network::reconcile(client.as_ref(), &mut account).await?;
                if account.identity().is_empty() {
                    return Err(account::expired());
                }
                Ok(json!({"ok":true,"credentials":account.credentials()}))
            }
            "device" => {
                let (mut pending, mut response) = auth::start(client.as_ref()).await?;
                let config = self.core.config()?;
                pending["networkProxy"] = params
                    .get("networkProxy")
                    .filter(|v| !v.is_null())
                    .cloned()
                    .unwrap_or(
                        serde_json::to_value(config.proxy)
                            .map_err(|_| PluginError::invalid("The Factory proxy is invalid."))?,
                    );
                let until = pending["until"].as_u64().unwrap();
                let session = self.core.begin(pending, until).await?;
                response["session"] = json!(session);
                Ok(response)
            }
            _ => Err(PluginError::unsupported(method)),
        }
    }
    async fn callback(&self, params: Value) -> PluginResult<Value> {
        let pending = self.core.take_pending(&params).await?;
        let client = self.params_client(&pending)?;
        let account = tokio::time::timeout(
            std::time::Duration::from_millis(1140000),
            auth::finish(client.as_ref(), pending),
        )
        .await
        .map_err(|_| account::expired())??;
        Ok(json!({"ok":true,"credentials":account.credentials()}))
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let client = self.params_client(&params)?;
        let account = self
            .live(client.as_ref(), params.get("credentials"), true)
            .await?;
        Ok(json!({"changed":true,"credentials":account.credentials()}))
    }
    async fn models(&self, _: CallContext) -> PluginResult<Value> {
        self.core.config()?;
        Ok(models::output(core::now()))
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let client = self.client(context.network_proxy.as_ref())?;
        let mut account = self
            .live(client.as_ref(), context.credentials.as_ref(), false)
            .await?;
        let mut repaired = false;
        loop {
            let response = network::send(
                client.as_ref(),
                network::factory(
                    HttpMethod::Get,
                    format!("{}/api/billing/limits", account.base()),
                    &account,
                )
                .with_header("accept", "application/json"),
            )
            .await?;
            let headers = response.headers.clone();
            let (status, value) = network::read(response).await?;
            if status == 403 && !repaired && !network::content_refused(status, &value, &headers) {
                repaired = true;
                if self.mend(client.as_ref(), &mut account, &value).await? {
                    continue;
                }
            }
            if !(200..300).contains(&status) {
                return Err(network::error(status));
            }
            return usage::parse(&value, &account, &context.account);
        }
    }
    async fn fetch(&self, original: FetchRequest) -> PluginResult<FetchResponse> {
        let mut parsed = request::parse(&original)?;
        let client = self.client(original.network_proxy.as_ref())?;
        let mut account = self
            .live(client.as_ref(), original.credentials.as_ref(), false)
            .await?;
        let mut repaired = false;
        let mut omitted = 0;
        loop {
            let response = network::send(
                client.as_ref(),
                self.outgoing(&parsed, &original, &account)?,
            )
            .await?;
            let status = response.status;
            if (200..300).contains(&status) {
                return stream::response(response, parsed.names.clone()).await;
            }
            if status == 413 {
                drop(response);
                if omitted == 0 && !repaired {
                    omitted = attachments::omit_older(&mut parsed);
                    if omitted > 0 {
                        continue;
                    }
                }
                return Ok(stream::too_large(parsed.wire, omitted));
            }
            let headers = response.headers.clone();
            let (_, value) = network::read(response).await?;
            if status == 403
                && !repaired
                && omitted == 0
                && !network::content_refused(status, &value, &headers)
            {
                repaired = true;
                if self.mend(client.as_ref(), &mut account, &value).await? {
                    continue;
                }
            }
            return Ok(stream::failure(parsed.wire, status));
        }
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
