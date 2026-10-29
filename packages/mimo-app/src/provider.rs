use crate::{
    auth, cookies,
    net::{self, Net, Reply, Request},
    readings,
};
use async_trait::async_trait;
use nora_ai::HttpMethod;
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, PluginError,
    PluginResult, ProviderPlugin,
};
use nora_provider_subscription_core::{self as core, Core};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::sync::Mutex;
#[derive(Clone)]
struct PlanKey {
    key: String,
    base: String,
    at: u64,
}
pub(crate) struct Mimo {
    core: Core,
    net: Arc<dyn Net>,
    platform: Mutex<BTreeMap<String, (u64, BTreeMap<String, String>)>>,
    keys: Mutex<BTreeMap<String, PlanKey>>,
    on_plan: Mutex<BTreeMap<String, u64>>,
}
impl Default for Mimo {
    fn default() -> Self {
        Self {
            core: Core::new("mimo-app"),
            net: Arc::new(net::Http),
            platform: Mutex::new(BTreeMap::new()),
            keys: Mutex::new(BTreeMap::new()),
            on_plan: Mutex::new(BTreeMap::new()),
        }
    }
}
fn stored_cookies(credentials: &Value) -> PluginResult<BTreeMap<String, String>> {
    serde_json::from_value(credentials["cookies"].clone()).map_err(|_| {
        PluginError::authentication("Xiaomi MiMo's saved session is invalid. Sign in again.")
    })
}
fn clean(headers: BTreeMap<String, String>) -> BTreeMap<String, String> {
    headers
        .into_iter()
        .filter(|(key, _)| {
            ![
                "content-length",
                "content-encoding",
                "transfer-encoding",
                "connection",
                "set-cookie",
            ]
            .contains(&key.to_ascii_lowercase().as_str())
        })
        .collect()
}
fn response(reply: Reply) -> FetchResponse {
    FetchResponse {
        status: reply.status,
        headers: clean(reply.headers),
        body: reply.body,
    }
}
fn credentials(stored: &Value) -> PluginResult<&Value> {
    if stored["provider"] != "mimo-app" || stored["type"] != "oauth" {
        return Err(PluginError::authentication(
            "Connect a Xiaomi MiMo account.",
        ));
    }
    let creds = &stored["xiaomi"];
    core::required(creds, "userId")?;
    core::required(creds, "passToken")?;
    core::required(creds, "deviceId")?;
    if auth::base(core::required(creds, "region")?).is_none() || stored["uid"] != creds["userId"] {
        return Err(PluginError::authentication(
            "Xiaomi MiMo's saved account binding is invalid.",
        ));
    }
    Ok(creds)
}
impl Mimo {
    fn proxy(&self, given: Option<&NetworkProxy>) -> PluginResult<Option<NetworkProxy>> {
        let proxy = given.cloned().or(self.core.config()?.proxy);
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        Ok(proxy)
    }
    async fn fresh(
        &self,
        stored: &Value,
        proxy: Option<&NetworkProxy>,
        force: bool,
    ) -> PluginResult<Value> {
        let _guard = self.core.refresh_lock.lock().await;
        let current = self.core.latest(stored).await?;
        let creds = credentials(&current)?.clone();
        let cookies = stored_cookies(&current)?;
        let expires = current["expires"].as_u64().unwrap_or(0);
        let newer = expires > stored["expires"].as_u64().unwrap_or(0)
            || current["cookies"] != stored["cookies"];
        if (!force || newer) && !cookies.is_empty() && expires > core::now() + 600_000 {
            return Ok(current);
        }
        match auth::session(
            self.net.as_ref(),
            &creds,
            core::required(&creds, "region")?,
            proxy,
        )
        .await
        {
            Ok(session) => {
                let mut next = auth::credential(creds, session.cookies, &session.me);
                for key in ["accountId", "label", "authMethod"] {
                    if !current[key].is_null() {
                        next[key] = current[key].clone();
                    }
                }
                self.core.persist_rotation(&current, next).await
            }
            Err(error)
                if error.code != "authentication_required" && !force && !cookies.is_empty() =>
            {
                Ok(current)
            }
            Err(error) => Err(error),
        }
    }
    async fn page(
        &self,
        stored: &Value,
        path: &str,
        proxy: Option<&NetworkProxy>,
    ) -> PluginResult<Value> {
        if !["/user/xiaomi/subscription/self", "/user/usage"].contains(&path) {
            return Err(PluginError::invalid(
                "Unsupported Xiaomi MiMo account page.",
            ));
        }
        for attempt in 0..=1 {
            let current = self.fresh(stored, proxy, attempt > 0).await?;
            let creds = credentials(&current)?;
            let mut headers = auth::app_headers();
            headers.insert(
                "cookie".into(),
                cookies::header(&stored_cookies(&current)?)?,
            );
            let (status, _, _, bytes) = net::read(
                self.net.as_ref(),
                Request::get(
                    format!(
                        "{}{path}",
                        auth::base(core::required(creds, "region")?).unwrap()
                    ),
                    headers,
                ),
                proxy,
                Duration::from_secs(20),
            )
            .await?;
            let envelope = serde_json::from_slice::<Value>(&bytes).ok();
            let bad = envelope.as_ref().is_none_or(|value| {
                !value.is_object()
                    || !value["code"].is_null()
                        && value["code"].as_f64().is_none_or(|code| code.fract() != 0.)
                    || !value["msg"].is_null() && !value["msg"].is_string()
            });
            if status == 401 || status == 200 && bad {
                if attempt == 0 {
                    continue;
                }
                return Err(PluginError::authentication(
                    "The Xiaomi MiMo session expired. Sign in again.",
                ));
            }
            if status != 200 || bad {
                return Err(PluginError::transport(format!(
                    "Xiaomi MiMo returned HTTP {status} for its account page."
                )));
            }
            let value = envelope.unwrap();
            if !value["code"].is_null() && value["code"].as_f64() != Some(0.) {
                return Err(PluginError::transport(
                    "Xiaomi MiMo refused its account page.",
                ));
            }
            return Ok(value.get("data").cloned().unwrap_or(Value::Null));
        }
        Err(PluginError::authentication(
            "The Xiaomi MiMo session expired.",
        ))
    }
    async fn platform_page(
        &self,
        creds: &Value,
        path: &str,
        proxy: Option<&NetworkProxy>,
    ) -> PluginResult<Value> {
        let uid = core::required(creds, "userId")?;
        let cached = self
            .platform
            .lock()
            .await
            .get(uid)
            .filter(|(at, _)| core::now().saturating_sub(*at) < 3_600_000)
            .map(|(_, cookies)| cookies.clone());
        let result =
            auth::platform_page(self.net.as_ref(), creds, path, cached.as_ref(), proxy).await;
        match result {
            Ok((cookies, value)) => {
                let mut platform = self.platform.lock().await;
                if platform.len() >= 256 {
                    platform.retain(|_, (at, _)| core::now().saturating_sub(*at) < 3_600_000);
                    if platform.len() >= 256 {
                        platform.clear();
                    }
                }
                platform.insert(uid.into(), (core::now(), cookies));
                Ok(value)
            }
            Err(error) => {
                self.platform.lock().await.remove(uid);
                Err(error)
            }
        }
    }
    async fn token_plan(
        &self,
        creds: &Value,
        app: bool,
        proxy: Option<&NetworkProxy>,
    ) -> Option<Value> {
        let detail = self
            .platform_page(creds, "/tokenPlan/detail", proxy)
            .await
            .ok()?;
        if detail["expired"] != false || core::text(&detail["planName"]).is_none() {
            return None;
        }
        let usage = self
            .platform_page(creds, "/tokenPlan/usage", proxy)
            .await
            .ok();
        readings::token_plan(&detail, usage.as_ref(), app)
    }
    async fn plan_key(&self, creds: &Value, proxy: Option<&NetworkProxy>) -> Option<PlanKey> {
        let uid = core::text(&creds["userId"])?;
        if let Some(key) = self
            .keys
            .lock()
            .await
            .get(uid)
            .filter(|key| core::now().saturating_sub(key.at) < 3_600_000)
        {
            return Some(key.clone());
        }
        self.keys.lock().await.remove(uid);
        let detail = self
            .platform_page(creds, "/tokenPlan/detail", proxy)
            .await
            .ok()?;
        if detail["expired"] != false || core::text(&detail["planName"]).is_none() {
            return None;
        }
        let info = self
            .platform_page(creds, "/tokenPlan/apiKey", proxy)
            .await
            .ok()?;
        let url = url::Url::parse(core::text(&info["openaiBaseUrl"])?).ok()?;
        if url.scheme() != "https"
            || !url
                .host_str()
                .is_some_and(|host| host.ends_with(".xiaomimimo.com"))
            || url.port_or_known_default() != Some(443)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return None;
        }
        let raw = self
            .platform_page(creds, "/tokenPlan/apiKey/raw", proxy)
            .await
            .ok()?;
        let key = raw.as_str().filter(|key| {
            key.starts_with("tp-") && key.len() <= 64 * 1024 && !key.chars().any(char::is_control)
        })?;
        let key = PlanKey {
            key: key.into(),
            base: url.as_str().trim_end_matches('/').into(),
            at: core::now(),
        };
        let mut keys = self.keys.lock().await;
        if keys.len() >= 256 {
            keys.retain(|_, key| core::now().saturating_sub(key.at) < 3_600_000);
            if keys.len() >= 256 {
                keys.clear();
            }
        }
        keys.insert(uid.into(), key.clone());
        Some(key)
    }
    async fn to_plan(
        &self,
        current: &Value,
        body: &bytes::Bytes,
        proxy: Option<&NetworkProxy>,
    ) -> PluginResult<Option<Reply>> {
        let creds = credentials(current)?;
        let Some(plan) = self.plan_key(creds, proxy).await else {
            return Ok(None);
        };
        let mut value: Value = serde_json::from_slice(body)
            .map_err(|_| PluginError::invalid("The MiMo Token Plan request is invalid JSON."))?;
        if let Some(model) = value["model"].as_str() {
            let actual = match model {
                "mimo-pro" => Some("mimo-v2.6-pro"),
                "mimo-flash" => Some("mimo-v2.6-flash"),
                _ => None,
            };
            if let Some(actual) = actual {
                value["model"] = json!(actual);
            }
        }
        let headers = BTreeMap::from([
            ("authorization".into(), format!("Bearer {}", plan.key)),
            ("content-type".into(), "application/json".into()),
            ("accept".into(), "*/*".into()),
        ]);
        let reply = self
            .net
            .send(
                Request {
                    method: HttpMethod::Post,
                    url: format!("{}/chat/completions", plan.base),
                    headers,
                    body: Some(
                        serde_json::to_vec(&value)
                            .map_err(|_| {
                                PluginError::invalid(
                                    "The MiMo Token Plan request could not be encoded.",
                                )
                            })?
                            .into(),
                    ),
                },
                proxy,
                Duration::from_secs(60),
            )
            .await?;
        if reply.status == 401 {
            self.keys
                .lock()
                .await
                .remove(core::required(creds, "userId")?);
        }
        Ok(Some(reply))
    }
}
#[async_trait]
impl ProviderPlugin for Mimo {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        self.core.initialize(params)
    }
    async fn auth_methods(&self, _: CallContext) -> PluginResult<Value> {
        Ok(self.core.config()?.manifest["contributes"]["provider"]["authMethods"].clone())
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        if core::required(&params, "authMethodID")? != "oauth" {
            return Err(PluginError::unsupported("authMethodID"));
        }
        let given = params
            .get("networkProxy")
            .cloned()
            .filter(|value| !value.is_null())
            .map(serde_json::from_value::<NetworkProxy>)
            .transpose()
            .map_err(|_| PluginError::invalid("Invalid Xiaomi MiMo network settings."))?;
        let proxy = self.proxy(given.as_ref())?;
        let login = auth::login(self.net.as_ref(), proxy.as_ref()).await?;
        let session = self
            .core
            .begin(
                json!({"poll":login.poll,"device":login.device}),
                login.until,
            )
            .await?;
        Ok(
            json!({"session":session,"url":login.url,"method":"auto","instructions":"Sign in to your Xiaomi account, or scan the page's QR code with the Xiaomi app. This completes automatically."}),
        )
    }
    async fn callback(&self, params: Value) -> PluginResult<Value> {
        let given = params
            .get("networkProxy")
            .cloned()
            .filter(|value| !value.is_null())
            .map(serde_json::from_value::<NetworkProxy>)
            .transpose()
            .map_err(|_| PluginError::invalid("Invalid Xiaomi MiMo network settings."))?;
        let proxy = self.proxy(given.as_ref())?;
        let credentials = tokio::time::timeout(Duration::from_millis(720_000), async {
            let ticket = self.core.take_pending(&params).await?;
            let value = auth::poll(self.net.as_ref(), &ticket, proxy.as_ref()).await?;
            auth::signed_in(
                self.net.as_ref(),
                &value,
                core::required(&ticket, "device")?,
                proxy.as_ref(),
            )
            .await
        })
        .await
        .map_err(|_| {
            PluginError::authentication("The Xiaomi MiMo sign-in timed out. Start again.")
        })??;
        Ok(json!({"ok":true,"credentials":credentials}))
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let given = params
            .get("networkProxy")
            .cloned()
            .filter(|value| !value.is_null())
            .map(serde_json::from_value::<NetworkProxy>)
            .transpose()
            .map_err(|_| PluginError::invalid("Invalid Xiaomi MiMo network settings."))?;
        let proxy = self.proxy(given.as_ref())?;
        self.fresh(
            params
                .get("credentials")
                .ok_or_else(|| PluginError::authentication("Connect a Xiaomi MiMo account."))?,
            proxy.as_ref(),
            params["force"] == true,
        )
        .await?;
        Ok(json!({"changed":false}))
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        let proxy = self.proxy(context.network_proxy.as_ref())?;
        let region = if let Some(stored) = context.credentials {
            let current = self.fresh(&stored, proxy.as_ref(), false).await?;
            core::required(credentials(&current)?, "region")?.to_owned()
        } else {
            "SGP".into()
        };
        let base = format!("{}/route", auth::base(&region).unwrap());
        let models = [("mimo-pro", "MiMo Pro"), ("mimo-flash", "MiMo Flash")]
            .into_iter()
            .map(|(id, name)| {
                let mut model = core::model(
                    id,
                    name,
                    "openai-completions",
                    "mimo-app",
                    &base,
                    1_000_000,
                    128_000,
                    true,
                    false,
                    Vec::new(),
                    "preset",
                );
                model["info"]["releaseDate"] = json!("2026-07-01");
                model
            })
            .collect();
        Ok(core::listing(models, "openai-completions", &base, "preset"))
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let proxy = self.proxy(context.network_proxy.as_ref())?;
        let stored = context
            .credentials
            .as_ref()
            .ok_or_else(|| PluginError::authentication("Connect Xiaomi MiMo to read usage."))?;
        let subscription = match self
            .page(stored, "/user/xiaomi/subscription/self", proxy.as_ref())
            .await
        {
            Ok(value) => value,
            Err(error) if error.code == "authentication_required" => return Err(error),
            Err(error) => {
                return Ok(
                    json!({"available":false,"account":{"id":context.account},"items":[],"reason":error.message}),
                )
            }
        };
        let current = self.core.latest(stored).await?;
        let creds = credentials(&current)?;
        let token_plan = self
            .token_plan(creds, subscription["current"].is_object(), proxy.as_ref())
            .await;
        let usage = match self.page(stored, "/user/usage", proxy.as_ref()).await {
            Ok(value) => Some(value),
            Err(error) if error.code == "authentication_required" => return Err(error),
            Err(_) => None,
        };
        Ok(readings::usage(
            &subscription,
            usage.as_ref(),
            token_plan,
            &context.account,
        ))
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        let url = core::validate_fetch(
            &request,
            auth::base("SGP").unwrap(),
            &["/api/route/chat/completions"],
        );
        let valid = url.is_ok()
            || ["RU", "IN"].iter().any(|region| {
                core::validate_fetch(
                    &request,
                    auth::base(region).unwrap(),
                    &["/api/route/chat/completions"],
                )
                .is_ok()
            });
        if !valid
            || url::Url::parse(&request.url)
                .ok()
                .is_some_and(|url| url.query().is_some())
        {
            return Err(PluginError::invalid(
                "This request is outside Xiaomi MiMo's chat endpoint.",
            ));
        }
        let proxy = self.proxy(request.network_proxy.as_ref())?;
        let current = self
            .fresh(
                request
                    .credentials
                    .as_ref()
                    .ok_or_else(|| PluginError::authentication("Connect a Xiaomi MiMo account."))?,
                proxy.as_ref(),
                false,
            )
            .await?;
        let creds = credentials(&current)?;
        let mut body: bytes::Bytes = request
            .decode_body()?
            .ok_or_else(|| PluginError::invalid("The Xiaomi MiMo request body is missing."))?
            .into();
        let mut value: Value = serde_json::from_slice(&body)
            .map_err(|_| PluginError::invalid("The Xiaomi MiMo request is invalid JSON."))?;
        core::required(&value, "model")?;
        if value["model"] == "mimo-auto" {
            value["model"] = json!("mimo-pro");
            body = serde_json::to_vec(&value).unwrap().into();
        }
        drop(value);
        let uid = core::required(creds, "userId")?;
        let preferred = self
            .on_plan
            .lock()
            .await
            .get(uid)
            .is_some_and(|at| core::now().saturating_sub(*at) < 3_600_000);
        if preferred {
            if let Some(reply) = self.to_plan(&current, &body, proxy.as_ref()).await? {
                return Ok(response(reply));
            }
            self.on_plan.lock().await.remove(uid);
        }
        let mut headers = auth::app_headers();
        headers.insert(
            "cookie".into(),
            cookies::header(&stored_cookies(&current)?)?,
        );
        headers.insert("x-mimo-source".into(), "mimocode-cli-free".into());
        headers.insert("content-type".into(), "application/json".into());
        headers.insert("accept".into(), "*/*".into());
        let reply = self
            .net
            .send(
                Request {
                    method: HttpMethod::Post,
                    url: format!(
                        "{}/route/chat/completions",
                        auth::base(core::required(creds, "region")?).unwrap()
                    ),
                    headers,
                    body: Some(body.clone()),
                },
                proxy.as_ref(),
                Duration::from_secs(60),
            )
            .await?;
        if reply.status == 403 {
            let (status, headers, _, bytes) = net::collect(reply, 16 * 1024 * 1024).await?;
            let membership = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .is_some_and(|value| value["error"]["code"] == "membership_required");
            if membership {
                if let Some(reply) = self.to_plan(&current, &body, proxy.as_ref()).await? {
                    let mut on_plan = self.on_plan.lock().await;
                    on_plan.retain(|_, at| core::now().saturating_sub(*at) < 3_600_000);
                    if on_plan.len() >= 256 {
                        on_plan.clear();
                    }
                    on_plan.insert(uid.into(), core::now());
                    return Ok(response(reply));
                }
            }
            return Ok(FetchResponse::bytes(status, clean(headers), bytes));
        }
        Ok(response(reply))
    }
}
#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
