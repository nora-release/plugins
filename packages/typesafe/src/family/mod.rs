use async_trait::async_trait;
use nora_ai::{HttpClient, HttpMethod, HttpRequest};
use nora_provider::definition::Definition;
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, PluginError,
    PluginResult, ProviderPlugin,
};
use nora_provider_subscription_core::{self as core, Core};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::sync::Mutex;
use url::Url;

mod catalog;
mod endpoints;
mod wire;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionFamily {
    Vercel,
    Cloudflare,
    Bailian,
    Qwen,
}
impl DecisionFamily {
    pub fn id(self) -> &'static str {
        match self {
            Self::Vercel => "vercel-jev",
            Self::Cloudflare => "cloudflare-jev",
            Self::Bailian => "bailian-decision",
            Self::Qwen => "qwen-decision",
        }
    }
}

pub struct DecisionProvider {
    family: DecisionFamily,
    core: Core,
    accounts: Mutex<BTreeMap<String, (u64, String)>>,
}
impl DecisionProvider {
    pub fn new(family: DecisionFamily) -> Self {
        Self {
            family,
            core: Core::new(family.id()),
            accounts: Mutex::default(),
        }
    }
    fn definition(&self) -> PluginResult<Definition> {
        Definition::parse(self.core.config()?.manifest)
    }
    fn api<'a>(&self, credentials: Option<&'a Value>) -> PluginResult<&'a Value> {
        let credentials = credentials
            .filter(|value| nora_provider::auth::is_api(value))
            .ok_or_else(|| {
                PluginError::authentication("Connect an API key for this decision provider.")
            })?;
        if credentials
            .get("provider")
            .and_then(Value::as_str)
            .is_some_and(|provider| provider != self.family.id())
        {
            return Err(PluginError::authentication(
                "This key belongs to another decision provider.",
            ));
        }
        core::bearer(credentials)?;
        Ok(credentials)
    }
    fn base(&self, credentials: Option<&Value>) -> PluginResult<String> {
        let definition = self.definition()?;
        let base = credentials
            .and_then(|value| value.get("baseUrl"))
            .and_then(Value::as_str)
            .unwrap_or(&definition.base_url);
        if self.family == DecisionFamily::Bailian
            && (base.is_empty() || base.contains("{WorkspaceId}"))
        {
            return Err(PluginError::authentication(
                "Reconnect this Bailian key with its workspace ID and region.",
            ));
        }
        endpoints::normalized(self.family, base)
    }
    fn client(&self, proxy: Option<&NetworkProxy>) -> PluginResult<Arc<dyn HttpClient>> {
        self.core.client(proxy)
    }
    async fn account_base(
        &self,
        base: &str,
        credentials: &Value,
        client: &dyn HttpClient,
    ) -> PluginResult<String> {
        let (api, selected) = endpoints::cloudflare(base)?;
        if let Some(selected) = selected.or_else(|| {
            credentials
                .get("accountId")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
        }) {
            return endpoints::cloudflare_run(&api, &selected);
        }
        let key = core::fingerprint(&json!({"api":api,"key":core::bearer(credentials)?}));
        {
            let mut accounts = self.accounts.lock().await;
            accounts.retain(|_, (at, _)| core::now().saturating_sub(*at) < 3_600_000);
            if let Some((_, account)) = accounts.get(&key) {
                return endpoints::cloudflare_run(&api, account);
            }
        }
        let request = request(
            HttpMethod::Get,
            endpoints::append(&api, "accounts")?,
            credentials,
            None,
        );
        let (status, _, bytes) =
            crate::provider::exchange(client, request, Duration::from_secs(20)).await?;
        if status == 403 {
            return Err(PluginError::authentication("This Cloudflare token cannot list accounts. Enter its account ID explicitly; Workers AI tokens need not have account-list permission."));
        }
        check(status)?;
        let value = json_value(&bytes)?;
        cloudflare_success(&value)?;
        let entries = value["result"]
            .as_array()
            .filter(|entries| entries.len() <= 4096)
            .ok_or_else(|| {
                PluginError::transport("Cloudflare returned an invalid account list.")
            })?;
        let account = entries.first().and_then(|value| value["id"].as_str()).filter(|id| endpoints::component(id)).ok_or_else(|| PluginError::authentication("The Cloudflare token lists no account. Enter an account ID that has Workers AI access."))?.to_owned();
        let mut accounts = self.accounts.lock().await;
        if accounts.len() >= 256 {
            if let Some(key) = accounts
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(key, _)| key.clone())
            {
                accounts.remove(&key);
            }
        }
        accounts.insert(key, (core::now(), account.clone()));
        endpoints::cloudflare_run(&api, &account)
    }
    async fn resolved(&self, credentials: &Value, client: &dyn HttpClient) -> PluginResult<String> {
        let base = self.base(Some(credentials))?;
        if self.family == DecisionFamily::Cloudflare {
            self.account_base(&base, credentials, client).await
        } else {
            Ok(base)
        }
    }
    fn presets(&self, base: &str) -> PluginResult<Value> {
        let definition = self.definition()?;
        Ok(catalog::result(
            definition
                .models
                .iter()
                .cloned()
                .map(|model| crate::provider::marked(model, &definition, base, "preset"))
                .collect(),
            base,
            "preset",
        ))
    }
    async fn list(&self, credentials: &Value, client: &dyn HttpClient) -> PluginResult<Value> {
        let base = self.resolved(credentials, client).await?;
        let definition = self.definition()?;
        match self.family {
            DecisionFamily::Bailian | DecisionFamily::Qwen => self.presets(&base),
            DecisionFamily::Vercel if endpoints::legacy(&base) => {
                let _ = credits(&base, credentials, client).await?;
                self.presets(&base)
            }
            DecisionFamily::Vercel => {
                let outgoing = request(
                    HttpMethod::Get,
                    endpoints::append(&base, "models")?,
                    credentials,
                    None,
                );
                let (status, _, bytes) =
                    crate::provider::exchange(client, outgoing, Duration::from_secs(20)).await?;
                check(status)?;
                let value = json_value(&bytes)?;
                let entries = value
                    .get("models")
                    .or_else(|| value.get("data"))
                    .or_else(|| value.is_array().then_some(&value))
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        PluginError::transport("Vercel returned an invalid decision model list.")
                    })?;
                Ok(catalog::result(
                    catalog::models(entries, &definition, &base, false)?,
                    &base,
                    "remote",
                ))
            }
            DecisionFamily::Cloudflare => {
                let (api, account) = endpoints::cloudflare(&base)?;
                let account = account.unwrap();
                let mut models = Vec::new();
                for page in 1..=16 {
                    let target = format!(
                        "{api}/accounts/{account}/ai/models/search?per_page=100&page={page}"
                    );
                    let outgoing = request(HttpMethod::Get, target, credentials, None);
                    let (status, _, bytes) =
                        crate::provider::exchange(client, outgoing, Duration::from_secs(20))
                            .await?;
                    check(status)?;
                    let value = json_value(&bytes)?;
                    cloudflare_success(&value)?;
                    let entries = value["result"].as_array().ok_or_else(|| {
                        PluginError::transport(
                            "Cloudflare returned an invalid Workers AI model list.",
                        )
                    })?;
                    if models.len().saturating_add(entries.len()) > 4096 {
                        return Err(PluginError::transport(
                            "Cloudflare returned too many Workers AI models.",
                        ));
                    }
                    models.extend(entries.iter().cloned());
                    let total = value
                        .pointer("/result_info/total_pages")
                        .and_then(Value::as_u64);
                    if total.is_some_and(|total| total <= page)
                        || total.is_none() && entries.len() < 100
                    {
                        return Ok(catalog::result(
                            catalog::models(&models, &definition, &base, true)?,
                            &base,
                            "remote",
                        ));
                    }
                }
                Err(PluginError::transport(
                    "Cloudflare's model list exceeded sixteen pages.",
                ))
            }
        }
    }
    async fn validate(&self, credentials: &Value, client: &dyn HttpClient) -> PluginResult<()> {
        if !matches!(self.family, DecisionFamily::Bailian | DecisionFamily::Qwen) {
            self.list(credentials, client).await?;
            return Ok(());
        }
        let base = self.base(Some(credentials))?;
        let body = serde_json::to_vec(&json!({"model":"decision-model-preview","state":{"text":"ping"},"questions":{"ok":{"type":"noul","instructions":"Is this a test?"}}})).unwrap();
        let outgoing = request(
            HttpMethod::Post,
            endpoints::append(&base, "systemone")?,
            credentials,
            Some(body),
        );
        let (status, _, bytes) =
            crate::provider::exchange(client, outgoing, Duration::from_secs(20)).await?;
        check(status)?;
        wire::answer(self.family, &base, &bytes)?;
        Ok(())
    }
}

fn request(
    method: HttpMethod,
    url: String,
    credentials: &Value,
    body: Option<Vec<u8>>,
) -> HttpRequest {
    let mut headers = BTreeMap::from([
        (
            "authorization".into(),
            format!(
                "Bearer {}",
                core::bearer(credentials).expect("validated API credentials")
            ),
        ),
        ("accept".into(), "application/json".into()),
    ]);
    if body.is_some() {
        headers.insert("content-type".into(), "application/json".into());
    }
    HttpRequest {
        method,
        url,
        headers,
        body,
    }
}
fn json_value(bytes: &[u8]) -> PluginResult<Value> {
    serde_json::from_slice(bytes)
        .map_err(|_| PluginError::transport("The decision provider returned invalid JSON."))
}
fn check(status: u16) -> PluginResult<()> {
    match status {
        200..=299 => Ok(()),
        401 | 403 => Err(PluginError::authentication(format!(
            "The decision provider refused this key (HTTP {status})."
        ))),
        _ => Err(PluginError::transport(format!(
            "The decision provider returned HTTP {status}."
        ))),
    }
}
fn cloudflare_success(value: &Value) -> PluginResult<()> {
    if value["success"] == false {
        return Err(PluginError::transport(
            "Cloudflare refused the account or model-list request.",
        ));
    }
    Ok(())
}
async fn credits(base: &str, credentials: &Value, client: &dyn HttpClient) -> PluginResult<Value> {
    let outgoing = request(
        HttpMethod::Get,
        endpoints::append(&endpoints::vercel_root(base)?, "v1/credits")?,
        credentials,
        None,
    );
    let (status, _, bytes) =
        crate::provider::exchange(client, outgoing, Duration::from_secs(20)).await?;
    check(status)?;
    json_value(&bytes)
}

#[async_trait]
impl ProviderPlugin for DecisionProvider {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        if params.manifest["api"] != "systemone"
            || params
                .manifest
                .pointer("/config/operations")
                .and_then(Value::as_array)
                .is_none_or(|operations| !operations.contains(&json!("decision")))
        {
            return Err(PluginError::invalid(
                "This executable requires its decision manifest.",
            ));
        }
        self.core.initialize(params)
    }
    async fn auth_methods(&self, _: CallContext) -> PluginResult<Value> {
        Ok(self.core.config()?.manifest["authMethods"].clone())
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        let method = params
            .get("authMethodID")
            .or_else(|| params.get("method"))
            .and_then(Value::as_str)
            .unwrap_or("apiKey");
        let definition = self.definition()?;
        if !definition
            .auth_methods
            .iter()
            .any(|entry| entry["id"] == method)
        {
            return Err(PluginError::unsupported("authMethodID"));
        }
        let inputs = core::inputs(&params);
        let key = core::required(inputs, "apiKey").or_else(|_| core::required(inputs, "key"))?;
        let mut credentials = json!({"provider":self.family.id(),"type":"apiKey","authMethod":method,"access":key,"expires":0});
        let base = if self.family == DecisionFamily::Bailian {
            endpoints::bailian(method, inputs)?
        } else {
            let supplied = inputs
                .get("endpoint")
                .and_then(core::text)
                .unwrap_or(&definition.base_url);
            endpoints::normalized(self.family, supplied)?
        };
        credentials["baseUrl"] = json!(base);
        let client = self.core.params_client(&params)?;
        if self.family == DecisionFamily::Cloudflare {
            let (_, embedded) = endpoints::cloudflare(&base)?;
            if let Some(account) = inputs.get("accountId").and_then(core::text) {
                if embedded
                    .as_deref()
                    .is_some_and(|embedded| embedded != account)
                {
                    return Err(PluginError::invalid(
                        "The entered Cloudflare account ID differs from the endpoint's account.",
                    ));
                }
                credentials["accountId"] = json!(account);
            }
            let resolved = self
                .account_base(&base, &credentials, client.as_ref())
                .await?;
            let (_, account) = endpoints::cloudflare(&resolved)?;
            credentials["accountId"] = json!(account.unwrap());
            credentials["baseUrl"] = json!(resolved);
        }
        self.validate(&credentials, client.as_ref()).await?;
        Ok(json!({"ok":true,"credentials":credentials}))
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        self.api(params.get("credentials"))?;
        Ok(json!({"changed":false}))
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        let Some(credentials) = context.credentials.as_ref() else {
            return self.presets(&self.definition()?.base_url);
        };
        let credentials = self.api(Some(credentials))?;
        let client = self.client(context.network_proxy.as_ref())?;
        self.list(credentials, client.as_ref()).await
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let credentials = self.api(context.credentials.as_ref())?;
        if self.family != DecisionFamily::Vercel {
            return Ok(
                json!({"available":false,"items":[],"message":"This decision provider does not expose a quota or balance API for this credential."}),
            );
        }
        let client = self.client(context.network_proxy.as_ref())?;
        let value = credits(&self.base(Some(credentials))?, credentials, client.as_ref()).await?;
        let finite = |value: &Value| {
            value
                .as_f64()
                .or_else(|| value.as_str()?.parse::<f64>().ok())
                .filter(|value| value.is_finite() && *value >= 0.0)
        };
        let balance = finite(&value["balance"])
            .ok_or_else(|| PluginError::transport("Vercel returned an invalid credit balance."))?;
        let mut items = Vec::new();
        if let Some(used) = finite(&value["total_used"]) {
            items.push(json!({"label":"Lifetime credits spent","used":used,"unit":"USD","usedPercent":null,"countsForRouting":false}));
        }
        Ok(json!({"available":true,"balance":format!("${balance:.2}"),"items":items}))
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        let credentials = self.api(request.credentials.as_ref())?;
        let base = self.base(Some(credentials))?;
        let target = endpoints::url(&request.url)?;
        let expected = endpoints::append(&base, "systemone")?;
        if !request.method.eq_ignore_ascii_case("POST") || target.as_str() != expected {
            return Err(PluginError::invalid(
                "This provider accepts only POST to its configured SystemOne endpoint.",
            ));
        }
        let bytes = crate::provider::validate_body(&request)?;
        let client = self.client(request.network_proxy.as_ref())?;
        let resolved = self.resolved(credentials, client.as_ref()).await?;
        let (target, bytes, headers) = wire::ask(self.family, &resolved, bytes)?;
        let mut outgoing = self::request(HttpMethod::Post, target, credentials, Some(bytes));
        outgoing.headers.extend(headers);
        let (status, headers, bytes) =
            crate::provider::exchange(client.as_ref(), outgoing, Duration::from_secs(8)).await?;
        let bytes = if status < 300 {
            wire::answer(self.family, &resolved, &bytes)?
        } else {
            bytes
        };
        Ok(FetchResponse::bytes(status, headers, bytes))
    }
}

#[cfg(test)]
#[path = "../../test/family.test.rs"]
mod tests;
