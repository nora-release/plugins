use crate::{auth, readings, stream};
use async_trait::async_trait;
use futures::StreamExt;
use nora_ai::{HttpClient, HttpMethod, HttpStreamingResponse};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, PluginError, PluginResult,
    ProviderPlugin,
};
use nora_provider_subscription_core::{self as core, Core};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
use tokio::sync::Mutex;
pub(crate) struct Raccoon {
    core: Core,
    browsers: Mutex<BTreeMap<String, auth::Browser>>,
    onboarding: Mutex<BTreeSet<String>>,
}
impl Default for Raccoon {
    fn default() -> Self {
        Self {
            core: Core::new("raccoon"),
            browsers: Mutex::new(BTreeMap::new()),
            onboarding: Mutex::new(BTreeSet::new()),
        }
    }
}
fn desktop_headers() -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "x-client-platform".into(),
            if cfg!(target_os = "macos") {
                "desktop-mac"
            } else {
                "desktop-windows"
            }
            .into(),
        ),
        ("x-client-version".into(), "v1.0.0".into()),
    ])
}
fn data(value: Value) -> PluginResult<Value> {
    if value["code"].as_i64().is_some_and(|n| n != 0) {
        let code = value["code"].as_i64().unwrap();
        if matches!(code, 200001 | 200003) {
            return Err(PluginError::authentication(
                "Raccoon refused this account. Sign in again.",
            ));
        }
        return Err(PluginError::transport(format!(
            "Raccoon refused the request (code {code})."
        )));
    }
    Ok(value.get("data").cloned().unwrap_or(value))
}
impl Raccoon {
    async fn account(
        &self,
        client: &dyn HttpClient,
        path: &str,
        credentials: &Value,
        method: HttpMethod,
        desktop: bool,
    ) -> PluginResult<Value> {
        let mut request = core::json_request(
            method,
            format!("{}{path}", auth::ORIGIN),
            None,
            Some(core::bearer(credentials)?),
        )?;
        if desktop {
            request.headers.extend(desktop_headers());
            request
                .headers
                .insert("x-raccoon-language".into(), "zh".into());
        }
        {
            let (status, value) = core::exchange(client, request).await?;
            if status == 401 {
                return Err(PluginError::authentication(
                    "Raccoon refused this account. Sign in again.",
                ));
            }
            if !(200..300).contains(&status) {
                return Err(PluginError::transport(format!(
                    "Raccoon returned HTTP {status}."
                )));
            }
            data(value)
        }
    }
    async fn live(
        &self,
        stored: &Value,
        client: &dyn HttpClient,
        force: bool,
    ) -> PluginResult<Value> {
        if stored["authMethod"] == "desktop" {
            if force {
                return Err(PluginError::authentication(
                    "Refresh this account in the Raccoon desktop app first.",
                ));
            }
            let path = core::required(&stored["source"], "path")?.to_owned();
            let expected = core::required(stored, "uid")?.to_owned();
            let live = tokio::task::spawn_blocking(move || auth::read_source(&path))
                .await
                .map_err(|_| {
                    PluginError::authentication("The selected Raccoon source could not be read.")
                })??;
            if live["uid"] != expected {
                return Err(PluginError::authentication("The selected Raccoon desktop file now belongs to another account. Reconnect it explicitly."));
            }
            return Ok(live);
        }
        let _guard = self.core.refresh_lock.lock().await;
        let current = self.core.latest(stored).await?;
        if current["provider"] != "raccoon" || current["type"] != "oauth" {
            return Err(PluginError::authentication(
                "Reconnect this Raccoon account.",
            ));
        }
        core::bearer(&current)?;
        let expiry = current["expires"].as_u64().unwrap_or(0);
        let newer = current["access"] != stored["access"];
        if (!force || newer) && (expiry == 0 || expiry > core::now() + 60_000) {
            return Ok(current);
        }
        let refresh = core::text(&current["refresh"]).ok_or_else(|| {
            PluginError::authentication(
                "Raccoon's token expired and cannot refresh. Sign in again.",
            )
        })?;
        let (status, response) = core::exchange(
            client,
            core::json_request(
                HttpMethod::Post,
                format!("{}/api/web/auth/v1/refresh", auth::ORIGIN),
                Some(&json!({"refresh_token":refresh})),
                None,
            )?,
        )
        .await?;
        if (400..500).contains(&status) && status != 429 {
            return Err(PluginError::authentication(
                "Raccoon refused the refresh token. Sign in again.",
            ));
        }
        if !(200..300).contains(&status) {
            return Err(PluginError::transport(format!(
                "Raccoon token renewal returned HTTP {status}; the account was kept."
            )));
        }
        let value = data(response)?;
        let next_access = core::required(&value, "access_token")?;
        let next_refresh = core::text(&value["refresh_token"]).unwrap_or(refresh);
        let mut next = auth::credential(
            next_access,
            next_refresh,
            current["authMethod"].as_str().unwrap_or("oauth"),
        )?;
        if core::text(&current["uid"]).is_some()
            && core::text(&next["uid"]).is_some()
            && current["uid"] != next["uid"]
        {
            return Err(PluginError::authentication(
                "Raccoon returned a different account while refreshing.",
            ));
        }
        for key in ["uid", "name", "label", "accountId", "onboardingSettled"] {
            if next[key].is_null()
                || core::text(&next[key]).is_none()
                || key == "accountId"
                || key == "onboardingSettled"
            {
                if !current[key].is_null() {
                    next[key] = current[key].clone();
                }
            }
        }
        self.core.persist_rotation(&current, next).await
    }
    async fn exchange_code(
        &self,
        client: &dyn HttpClient,
        code: &str,
        method: &str,
    ) -> PluginResult<Value> {
        let value = data(
            core::checked(
                client,
                core::json_request(
                    HttpMethod::Post,
                    format!(
                        "{}/api/web/auth/v1/login_with_authorization_code",
                        auth::ORIGIN
                    ),
                    Some(&json!({"authorization_code":code})),
                    None,
                )?,
            )
            .await?,
        )?;
        let credential = auth::credential(
            core::required(&value, "access_token")?,
            core::required(&value, "refresh_token")?,
            method,
        )?;
        self.account(
            client,
            "/api/web/points/v1/balance",
            &credential,
            HttpMethod::Get,
            false,
        )
        .await?;
        Ok(credential)
    }
    async fn bills(&self, client: &dyn HttpClient, credentials: &Value) -> PluginResult<Value> {
        let mut result = Value::Null;
        for limit in [1000, 5000] {
            result = self
                .account(
                    client,
                    &format!("/api/web/points/v1/bills?paging.limit={limit}&paging.offset=0"),
                    credentials,
                    HttpMethod::Get,
                    false,
                )
                .await?;
            if result["items"]
                .as_array()
                .is_none_or(|items| items.len() < limit)
            {
                break;
            }
        }
        Ok(result)
    }
    async fn usage_read(
        &self,
        client: &dyn HttpClient,
        credentials: &Value,
        account: &str,
    ) -> PluginResult<Value> {
        let points = self
            .account(
                client,
                "/api/web/points/v1/balance",
                credentials,
                HttpMethod::Get,
                false,
            )
            .await?;
        let entitlement = self
            .account(
                client,
                "/api/web/auth/v1/entitlement_info",
                credentials,
                HttpMethod::Get,
                false,
            )
            .await
            .unwrap_or(Value::Null);
        let bills = self.bills(client, credentials).await.unwrap_or(Value::Null);
        let mut value = readings::usage(&points, &entitlement, &bills, account)?;
        value["account"]["name"] = credentials["name"].clone();
        Ok(value)
    }
    async fn send(
        &self,
        client: &dyn HttpClient,
        request: &FetchRequest,
        credentials: &Value,
    ) -> PluginResult<HttpStreamingResponse> {
        let outgoing = core::json_request(
            HttpMethod::Post,
            &request.url,
            None,
            Some(core::bearer(credentials)?),
        )?
        .with_header("content-type", "application/json")
        .with_header("accept", "*/*")
        .with_body(
            request
                .decode_body()?
                .ok_or_else(|| PluginError::invalid("The Raccoon request body is missing."))?,
        );
        tokio::time::timeout(Duration::from_secs(60), client.stream(outgoing))
            .await
            .map_err(|_| PluginError::transport("Raccoon did not answer in time."))?
            .map_err(|_| PluginError::transport("Raccoon could not be reached."))
    }
    async fn checkin(&self, client: &dyn HttpClient, credentials: &Value) -> PluginResult<Value> {
        let day = readings::today();
        let path = "/api/web/points/v1/bills?paging.limit=20&paging.offset=0";
        let before = self
            .account(client, path, credentials, HttpMethod::Get, false)
            .await
            .ok()
            .and_then(|value| value["items"].as_array().cloned());
        let before = before.map(|items| {
            items
                .iter()
                .filter(|bill| readings::grants(bill, &day))
                .map(readings::bill_key)
                .collect::<BTreeSet<_>>()
        });
        self.account(
            client,
            "/api/web/office/v3/setting_info",
            credentials,
            HttpMethod::Get,
            true,
        )
        .await?;
        let key = core::text(&credentials["uid"])
            .map(str::to_owned)
            .unwrap_or_else(|| core::fingerprint(credentials));
        let settled =
            credentials["onboardingSettled"] == true || self.onboarding.lock().await.contains(&key);
        let mut onboarding = 0.0;
        let mut explicitly_claimed = false;
        if !settled {
            if let Ok(grant) = self
                .account(
                    client,
                    "/api/web/desktop/v1/login/points/grant",
                    credentials,
                    HttpMethod::Post,
                    true,
                )
                .await
            {
                if grant["granted"].is_boolean() {
                    if grant["granted"] == true {
                        explicitly_claimed = true;
                        onboarding = readings::number(&grant["popup"]["points"])
                            .filter(|n| *n > 0.0)
                            .unwrap_or(0.0)
                            .floor();
                    }
                    if credentials["authMethod"] == "desktop" {
                        let mut done = self.onboarding.lock().await;
                        if done.len() >= 1024 {
                            done.clear();
                        }
                        done.insert(key);
                    } else {
                        let mut next = credentials.clone();
                        next["onboardingSettled"] = json!(true);
                        self.core.persist_rotation(credentials, next).await?;
                    }
                }
            }
        }
        let Some(before) = before else {
            return Ok(if explicitly_claimed {
                json!({"outcome":"claimed","credit":if onboarding>0.0{Some(onboarding)}else{None}})
            } else {
                json!({"outcome":"failed","message":"The provider accepted the check-in, but its reward could not be verified."})
            });
        };
        let bills = self
            .account(client, path, credentials, HttpMethod::Get, false)
            .await?;
        let mut total = 0.0;
        let mut has_reward = false;
        for bill in bills["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|bill| readings::grants(bill, &day))
        {
            if before.contains(&readings::bill_key(bill)) {
                continue;
            }
            total += readings::number(&bill["points"])
                .unwrap_or(0.0)
                .max(0.0)
                .floor();
            if bill["biz_type"] == "reward_grant" {
                has_reward = true;
            }
        }
        if onboarding > 0.0 && !has_reward {
            total += onboarding;
        }
        Ok(if total > 0.0 {
            json!({"outcome":"claimed","credit":total})
        } else if explicitly_claimed {
            json!({"outcome":"claimed","message":"The provider confirmed a reward without reporting an amount."})
        } else {
            json!({"outcome":"done"})
        })
    }
}
#[async_trait]
impl ProviderPlugin for Raccoon {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        self.core.initialize(params)
    }
    async fn auth_methods(&self, _: CallContext) -> PluginResult<Value> {
        Ok(self.core.config()?.manifest["contributes"]["provider"]["authMethods"].clone())
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        let client = self.core.params_client(&params)?;
        match core::required(&params, "authMethodID")? {
            "desktop" => {
                let path = core::required(core::inputs(&params), "sourcePath")?.to_owned();
                let live = tokio::task::spawn_blocking(move || auth::read_source(&path))
                    .await
                    .map_err(|_| {
                        PluginError::authentication(
                            "The selected Raccoon account could not be read.",
                        )
                    })??;
                self.account(
                    client.as_ref(),
                    "/api/web/points/v1/balance",
                    &live,
                    HttpMethod::Get,
                    false,
                )
                .await?;
                Ok(
                    json!({"ok":true,"credentials":{"type":"oauth","authMethod":"desktop","provider":"raccoon","source":live["source"],"uid":live["uid"],"name":live["name"],"accountId":live["uid"],"label":live["label"],"expires":0}}),
                )
            }
            "manual" => {
                let state = auth::state();
                let url = auth::authorize_url(&state, None)?;
                let session = self
                    .core
                    .begin(
                        json!({"method":"manual","state":state}),
                        core::now() + 600_000,
                    )
                    .await?;
                Ok(
                    json!({"session":session,"url":url,"method":"code","instructions":"Complete Raccoon sign-in, then paste the office-raccoon callback URL or JWT token. This is not an SMS verification code."}),
                )
            }
            "oauth" => {
                let state = auth::state();
                let (url, browser) = auth::browser(state.clone()).await?;
                let mut browsers = self.browsers.lock().await;
                browsers.retain(|_, pending| pending.until > core::now());
                if browsers.len() >= 16 {
                    return Err(PluginError::invalid(
                        "Finish an earlier Raccoon browser sign-in first.",
                    ));
                }
                let session = self
                    .core
                    .begin(json!({"method":"browser","state":state}), browser.until)
                    .await?;
                browsers.insert(session.clone(), browser);
                Ok(
                    json!({"session":session,"url":url,"method":"auto","instructions":"Complete Raccoon sign-in in your browser. It returns here automatically."}),
                )
            }
            method => Err(PluginError::unsupported(method)),
        }
    }
    async fn callback(&self, params: Value) -> PluginResult<Value> {
        let session = core::required(&params, "session")?;
        let pending = self
            .core
            .pending
            .lock()
            .await
            .get(session)
            .filter(|p| p["until"].as_u64().unwrap_or(0) > core::now())
            .cloned()
            .ok_or_else(|| {
                PluginError::authentication("The Raccoon sign-in expired. Start again.")
            })?;
        let client = self.core.params_client(&params)?;
        let credentials = if pending["method"] == "browser" {
            self.core.pending.lock().await.remove(session);
            let mut browser = self.browsers.lock().await.remove(session).ok_or_else(|| {
                PluginError::authentication("The Raccoon browser sign-in expired.")
            })?;
            let code = (&mut browser.receive).await.map_err(|_| {
                PluginError::authentication("The Raccoon browser sign-in was cancelled.")
            })??;
            self.exchange_code(client.as_ref(), &code, "oauth").await?
        } else {
            let raw = core::text(&params["code"])
                .or_else(|| core::text(&params["input"]))
                .ok_or_else(|| {
                    PluginError::invalid("Paste the Raccoon callback URL or JWT token.")
                })?;
            let parsed = auth::manual(raw, core::required(&pending, "state")?)?;
            if let Some(direct) = parsed.get("direct") {
                self.account(
                    client.as_ref(),
                    "/api/web/points/v1/balance",
                    direct,
                    HttpMethod::Get,
                    false,
                )
                .await?;
                direct.clone()
            } else {
                self.exchange_code(client.as_ref(), core::required(&parsed, "code")?, "manual")
                    .await?
            }
        };
        self.core.pending.lock().await.remove(session);
        Ok(json!({"ok":true,"credentials":credentials}))
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let client = self.core.params_client(&params)?;
        let credentials = params
            .get("credentials")
            .ok_or_else(|| PluginError::authentication("Connect this Raccoon account."))?;
        self.live(credentials, client.as_ref(), params["force"] == true)
            .await?;
        Ok(json!({"changed":false}))
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        let client = self.core.client(context.network_proxy.as_ref())?;
        let credentials = if let Some(stored) = &context.credentials {
            Some(self.live(stored, client.as_ref(), false).await?)
        } else {
            None
        };
        let request = core::json_request(
            HttpMethod::Get,
            format!("{}/model_catalog", auth::BASE),
            None,
            credentials.as_ref().map(core::bearer).transpose()?,
        )?;
        match core::checked(client.as_ref(), request)
            .await
            .and_then(|value| readings::models(&value))
        {
            Ok(models) => Ok(core::listing(
                models,
                "openai-completions",
                auth::BASE,
                "remote",
            )),
            Err(_) if credentials.is_none() => Ok(core::listing(
                readings::presets(),
                "openai-completions",
                auth::BASE,
                "preset",
            )),
            Err(error) => Err(error),
        }
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let client = self.core.client(context.network_proxy.as_ref())?;
        let stored = context.credentials.as_ref().ok_or_else(|| {
            PluginError::authentication("Connect a Raccoon account to read usage.")
        })?;
        let credentials = self.live(stored, client.as_ref(), false).await?;
        match self
            .usage_read(client.as_ref(), &credentials, &context.account)
            .await
        {
            Err(error)
                if error.code == "authentication_required"
                    && core::text(&credentials["refresh"]).is_some()
                    && credentials["authMethod"] != "desktop" =>
            {
                let next = self.live(stored, client.as_ref(), true).await?;
                self.usage_read(client.as_ref(), &next, &context.account)
                    .await
            }
            result => result,
        }
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        core::validate_fetch(&request, auth::BASE, &["/api/web/llm/v2/chat/completions"])?;
        let body = request
            .decode_body()?
            .ok_or_else(|| PluginError::invalid("The Raccoon request body is missing."))?;
        let value: Value = serde_json::from_slice(&body)
            .map_err(|_| PluginError::invalid("The Raccoon request is invalid JSON."))?;
        let requested = core::required(&value, "model")?.to_owned();
        drop(body);
        let client = self.core.client(request.network_proxy.as_ref())?;
        let stored = request
            .credentials
            .as_ref()
            .ok_or_else(|| PluginError::authentication("Connect a Raccoon account."))?;
        let credentials = self.live(stored, client.as_ref(), false).await?;
        let mut response = self.send(client.as_ref(), &request, &credentials).await?;
        if response.status == 401
            && core::text(&credentials["refresh"]).is_some()
            && credentials["authMethod"] != "desktop"
        {
            drop(response);
            let next = self.live(stored, client.as_ref(), true).await?;
            response = self.send(client.as_ref(), &request, &next).await?;
        }
        let status = response.status;
        let mut headers = response.headers.clone();
        headers.retain(|key, _| {
            ![
                "content-length",
                "content-encoding",
                "transfer-encoding",
                "connection",
                "set-cookie",
            ]
            .contains(&key.to_ascii_lowercase().as_str())
        });
        let content_type = headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("content-type"))
            .map(|(_, value)| value.as_str())
            .unwrap_or("");
        if content_type.contains("application/json") {
            let bytes = core::collect(response, 32 * 1024 * 1024).await?;
            let mut value = serde_json::from_slice::<Value>(&bytes).ok();
            if let Some(value) = value
                .as_mut()
                .filter(|value| value.is_object() && value.get("model").is_some())
            {
                value["model"] = json!(requested);
                return Ok(FetchResponse::bytes(
                    status,
                    headers,
                    serde_json::to_vec(value).unwrap(),
                ));
            }
            return Ok(FetchResponse::bytes(status, headers, bytes));
        }
        let body = if content_type.contains("text/event-stream") {
            stream::rewrite(response.body, requested)
        } else {
            Box::pin(response.body.map(|chunk| {
                chunk
                    .map(|bytes| bytes.to_vec())
                    .map_err(|_| PluginError::transport("Raccoon's response stopped."))
            })) as nora_provider_plugin_sdk::ResponseBody
        };
        Ok(FetchResponse {
            status,
            headers,
            body,
        })
    }
    async fn call(&self, method: &str, params: Value) -> PluginResult<Value> {
        if method != "checkin" {
            return Err(PluginError::unsupported(method));
        }
        let client = self.core.params_client(&params)?;
        let stored = params
            .get("credentials")
            .ok_or_else(|| PluginError::authentication("Connect Raccoon before checking in."))?;
        let credentials = self.live(stored, client.as_ref(), false).await?;
        match self.checkin(client.as_ref(), &credentials).await {
            Err(error)
                if error.code == "authentication_required"
                    && core::text(&credentials["refresh"]).is_some()
                    && credentials["authMethod"] != "desktop" =>
            {
                let next = self.live(stored, client.as_ref(), true).await?;
                self.checkin(client.as_ref(), &next).await
            }
            result => result,
        }
    }
}
#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
