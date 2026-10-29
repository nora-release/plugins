use crate::{
    auth, catalog, device,
    endpoint::{self, Site},
    readings::{self, Gift},
    start, tool_search,
};
use async_trait::async_trait;
use futures::StreamExt;
use nora_ai::{HttpClient, HttpMethod, HttpRequest};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, PluginError, PluginResult,
    ProviderPlugin,
};
use nora_provider_subscription_core::{self as core, Core};
use rand::RngCore;
use serde_json::{json, Value};
use std::{collections::BTreeMap, time::Duration};
use tokio::sync::Mutex;
#[derive(Clone)]
struct Route {
    start: bool,
    gift: Option<Gift>,
    at: u64,
    ttl: u64,
}
pub(crate) struct Zcode {
    core: Core,
    device: String,
    routes: Mutex<BTreeMap<String, Route>>,
    blocked: Mutex<BTreeMap<String, u64>>,
    config: Mutex<Option<(u64, Value)>>,
}
impl Default for Zcode {
    fn default() -> Self {
        Self {
            core: Core::new("zcode"),
            device: uuid::Uuid::new_v4().to_string(),
            routes: Mutex::new(BTreeMap::new()),
            blocked: Mutex::new(BTreeMap::new()),
            config: Mutex::new(None),
        }
    }
}
fn site(state: &Value) -> PluginResult<Site> {
    Site::parse(core::required(state, "site")?)
}
fn team(state: &Value) -> bool {
    core::text(&state["org"]).is_some() && core::text(&state["project"]).is_some()
}
fn route_key(state: &Value) -> String {
    core::fingerprint(&json!([state["key"], state["jwt"]]))
}
fn blocked_key(state: &Value, model: &str) -> String {
    core::fingerprint(&json!([state["key"], state["jwt"], model.to_lowercase()]))
}
fn clean(headers: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    headers
        .iter()
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
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}
impl Zcode {
    async fn selected_state(&self, stored: &Value) -> PluginResult<Value> {
        let current = self.core.latest(stored).await?;
        if current["provider"] != "zcode" {
            return Err(PluginError::authentication("Reconnect this ZCode account."));
        }
        let mut state = if current["authMethod"] == "source" {
            let inputs = current["source"].clone();
            let device = core::text(&current["device"]).unwrap_or("").to_owned();
            let state = tokio::task::spawn_blocking(move || auth::source(&inputs, &device))
                .await
                .map_err(|_| {
                    PluginError::authentication("The selected ZCode source could not be read.")
                })??;
            if state["uid"] != current["uid"]
                || state["site"] != current["site"]
                || state["org"] != current["org"]
                || state["project"] != current["project"]
            {
                return Err(PluginError::authentication("The selected ZCode files now identify a different account or plan. Reconnect them explicitly."));
            }
            state
        } else {
            current["state"].clone()
        };
        site(&state)?;
        if core::text(&state["device"]).is_none() {
            state["device"] = json!(core::text(&state["key"])
                .map(device::key_id)
                .unwrap_or_else(|| self.device.clone()));
        }
        if core::text(&state["key"]).is_none()
            && core::text(&state["jwt"]).is_none()
            && !team(&state)
        {
            return Err(PluginError::authentication(
                "ZCode's sign-in has no usable plan key or session.",
            ));
        }
        Ok(state)
    }
    async fn state(&self, stored: &Value, client: &dyn HttpClient) -> PluginResult<Value> {
        let mut state = self.selected_state(stored).await?;
        if team(&state) && core::text(&state["key"]).is_none() {
            let key = auth::project_key(
                client,
                site(&state)?,
                core::required(&state, "token")?,
                core::required(&state, "org")?,
                core::required(&state, "project")?,
                true,
                false,
                core::required(&state, "device")?,
            )
            .await?;
            state["key"] = json!(key);
            let current = self.core.latest(stored).await?;
            if current["authMethod"] != "source" {
                let mut next = current.clone();
                next["state"] = state.clone();
                next["access"] = state["key"].clone();
                self.core.persist_rotation(&current, next).await?;
            }
        }
        Ok(state)
    }
    async fn call(
        &self,
        client: &dyn HttpClient,
        state: &Value,
        method: HttpMethod,
        url: &str,
        auth: Option<&str>,
        headers: BTreeMap<String, String>,
    ) -> PluginResult<Value> {
        Ok(endpoint::call(
            client,
            method,
            url,
            auth,
            None,
            headers,
            state["device"].as_str().unwrap_or(&self.device),
        )
        .await?)
    }
    async fn balance(
        &self,
        client: &dyn HttpClient,
        state: &Value,
        start: bool,
    ) -> PluginResult<Option<Gift>> {
        let jwt = core::required(state, "jwt")?;
        endpoint::live_jwt(jwt)?;
        let value = self
            .call(
                client,
                state,
                HttpMethod::Get,
                &format!(
                    "{}/api/v1/zcode-plan/billing/balance?app_version={}",
                    endpoint::ZCODE,
                    endpoint::VERSION
                ),
                Some(&format!("Bearer {jwt}")),
                BTreeMap::new(),
            )
            .await?;
        Ok(readings::gift(&value, start))
    }
    async fn route(&self, client: &dyn HttpClient, state: &Value) -> Route {
        if core::text(&state["jwt"]).is_none() {
            return Route {
                start: false,
                gift: None,
                at: core::now(),
                ttl: 600_000,
            };
        }
        if core::text(&state["key"]).is_none() && !team(state) {
            return Route {
                start: true,
                gift: None,
                at: core::now(),
                ttl: 600_000,
            };
        }
        let key = route_key(state);
        if let Some(route) = self
            .routes
            .lock()
            .await
            .get(&key)
            .filter(|route| core::now().saturating_sub(route.at) < route.ttl)
        {
            return route.clone();
        }
        let (start, sure) = if team(state) {
            // A team seat is independent of the person's subscription list.
            (false, true)
        } else {
            let selected = site(state).unwrap();
            let plan = self
                .call(
                    client,
                    state,
                    HttpMethod::Get,
                    &format!("{}/api/biz/subscription/list", selected.root()),
                    core::text(&state["key"]),
                    BTreeMap::new(),
                )
                .await;
            (
                plan.as_ref()
                    .is_ok_and(|value| readings::plan(value).is_none()),
                plan.is_ok(),
            )
        };
        let (gift, sure) = if start {
            (None, sure)
        } else {
            let gift = self.balance(client, state, false).await;
            let known = gift.is_ok()
                || core::text(&state["jwt"]).is_some_and(|jwt| endpoint::live_jwt(jwt).is_err());
            (gift.ok().flatten(), sure && known)
        };
        let route = Route {
            start,
            gift: gift.clone(),
            at: core::now(),
            ttl: (if sure { 600_000u64 } else { 60_000u64 })
                .min(gift.as_ref().map(|gift| gift.ttl).unwrap_or(600_000)),
        };
        let mut routes = self.routes.lock().await;
        if routes.len() >= 512 {
            routes.retain(|_, route| core::now().saturating_sub(route.at) < route.ttl);
            if routes.len() >= 512 {
                routes.clear();
            }
        }
        routes.insert(key, route.clone());
        route
    }
    async fn complete(&self, params: &Value) -> PluginResult<Value> {
        let pending = self.core.take_pending(params).await?;
        let selected = Site::parse(core::required(&pending, "site")?)?;
        let poll = core::required(&pending, "poll")?;
        let device = core::required(&pending, "device")?;
        let flow = core::required(&pending, "flow")?;
        let interval = pending["interval"]
            .as_u64()
            .unwrap_or(1000)
            .clamp(1000, 30000);
        let until = pending["until"].as_u64().unwrap_or(0);
        let client = self.core.params_client(params)?;
        loop {
            if core::now() >= until {
                return Err(PluginError::authentication(
                    "The ZCode sign-in expired. Start again.",
                ));
            }
            tokio::time::sleep(Duration::from_millis(
                interval.min(until.saturating_sub(core::now())),
            ))
            .await;
            let result = endpoint::call(
                client.as_ref(),
                HttpMethod::Get,
                &format!(
                    "{}/api/v1/oauth/cli/poll/{}",
                    endpoint::ZCODE,
                    core::encode(flow)
                ),
                Some(poll),
                None,
                BTreeMap::new(),
                device,
            )
            .await;
            let got = match result {
                Ok(got) => got,
                Err(error)
                    if (400..500).contains(&error.status)
                        && error.status != 408
                        && error.status != 429 =>
                {
                    return Err(error.into())
                }
                Err(_) => continue,
            };
            match got["status"].as_str() {
                None | Some("pending") => continue,
                Some("failed") => {
                    return Err(PluginError::new(
                        "authorization_denied",
                        "The ZCode sign-in was declined.",
                    ))
                }
                Some("ready") => {}
                _ => {
                    return Err(PluginError::authentication(
                        "ZCode returned an unexpected sign-in state.",
                    ))
                }
            }
            let token = if selected == Site::Zai {
                core::text(&got["zai"]["access_token"])
            } else {
                core::text(&got["bigmodel"]["access_token"])
                    .or_else(|| core::text(&got["bigmodel"]["accessToken"]))
            }
            .ok_or_else(|| PluginError::authentication("ZCode returned no sign-in token."))?;
            let jwt = core::text(&got["token"]).unwrap_or("");
            let state = auth::sign_in(
                client.as_ref(),
                selected,
                token,
                jwt,
                device,
                pending["team"] == true,
            )
            .await?;
            let uid = auth::identity(&got["user"]);
            if uid.is_empty() {
                return Err(PluginError::authentication(
                    "ZCode did not identify the signed-in account.",
                ));
            }
            let account = if pending["team"] == true {
                format!("{uid} (team)")
            } else {
                uid.clone()
            };
            let credentials = json!({"provider":"zcode","type":"oauth","authMethod":pending["method"],"access":core::text(&state["key"]).or_else(||core::text(&state["jwt"])).unwrap_or(""),"expires":if core::text(&state["key"]).is_some()||team(&state){0}else{endpoint::expiry(jwt)},"uid":uid,"accountId":account,"label":account,"site":selected.id(),"state":state});
            return Ok(credentials);
        }
    }
    async fn config(
        &self,
        client: &dyn HttpClient,
        stored: Option<&Value>,
    ) -> (Option<Value>, &'static str) {
        let previous = self.config.lock().await.clone();
        let remote = if let Some((at, value)) =
            previous.filter(|(at, _)| core::now().saturating_sub(*at) < 600_000)
        {
            let _ = at;
            Some(value)
        } else {
            let value = tokio::time::timeout(
                Duration::from_secs(8),
                catalog::remote(client, &self.device),
            )
            .await
            .ok()
            .and_then(Result::ok);
            if let Some(value) = &value {
                *self.config.lock().await = Some((core::now(), value.clone()));
            }
            value
        };
        let local = stored
            .filter(|stored| stored["authMethod"] == "source")
            .map(|stored| stored["source"].clone());
        let local = if let Some(source) = local {
            tokio::task::spawn_blocking(move || catalog::local(&source))
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
        } else {
            None
        };
        match (remote, local) {
            (Some(remote), Some(local))
                if local["revision"].as_u64().unwrap_or(0)
                    > remote["revision"].as_u64().unwrap_or(0) =>
            {
                (Some(local), "local")
            }
            (Some(remote), _) => (Some(remote), "remote"),
            (None, Some(local)) => (Some(local), "local"),
            _ => (None, "preset"),
        }
    }
    async fn coding_usage(
        &self,
        client: &dyn HttpClient,
        state: &Value,
        account: &str,
    ) -> PluginResult<Value> {
        let selected = site(state)?;
        let team = team(state);
        let root = if team {
            selected.api()
        } else {
            selected.root()
        };
        let headers = if team {
            endpoint::team_headers(
                selected,
                core::required(state, "org")?,
                core::required(state, "project")?,
            )
        } else {
            BTreeMap::new()
        };
        let limits = self
            .call(
                client,
                state,
                HttpMethod::Get,
                &format!(
                    "{root}/api/monitor/usage/quota/limit{}",
                    if team { "?type=2" } else { "" }
                ),
                core::text(&state["key"]),
                headers.clone(),
            )
            .await?;
        let level = core::text(&limits["level"]);
        let mut value = json!({"available":true,"account":{"id":account},"plan":level.map(|level|format!("GLM Coding {level}")).or_else(||state["plan"].as_str().map(str::to_owned)),"items":readings::limits(&limits)});
        if team {
            if let Some(token) = core::text(&state["token"]) {
                if let Ok(detail) = self
                    .call(
                        client,
                        state,
                        HttpMethod::Get,
                        &format!("{root}/api/biz/team/subscribe/product/querySubscribeDetail"),
                        Some(token),
                        headers.clone(),
                    )
                    .await
                {
                    if detail["hasSubscription"] != false
                        && detail["status"]
                            .as_str()
                            .is_some_and(|s| s.eq_ignore_ascii_case("EFFECTIVE"))
                        && detail["memberGrantStatus"]
                            .as_str()
                            .is_some_and(|s| s.eq_ignore_ascii_case("VALID"))
                    {
                        if !detail["productName"].is_null() {
                            value["plan"] = detail["productName"].clone();
                        }
                        value["until"] = readings::when(&detail["subscribeEndTime"])
                            .map(core::iso)
                            .unwrap_or(Value::Null);
                        value["renew"] = json!("off");
                    }
                }
            }
        } else if let Ok(subs) = self
            .call(
                client,
                state,
                HttpMethod::Get,
                &format!("{root}/api/biz/subscription/list"),
                core::text(&state["key"]),
                BTreeMap::new(),
            )
            .await
        {
            value
                .as_object_mut()
                .unwrap()
                .extend(readings::term(&subs).as_object().unwrap().clone());
        }
        let reset_auth = if team {
            core::text(&state["token"])
        } else {
            core::text(&state["key"])
        };
        if let Some(auth) = reset_auth {
            if let Ok(resets) = self
                .call(
                    client,
                    state,
                    HttpMethod::Get,
                    &format!(
                        "{root}/api/biz/customer-package-reset/list?targetType={}",
                        if team { "TEAM" } else { "PERSONAL" }
                    ),
                    Some(auth),
                    headers,
                )
                .await
            {
                if let Some(resets) = readings::resets(&resets, team) {
                    value["resets"] = resets;
                }
            }
        }
        Ok(value)
    }

    async fn include_gifts(&self, client: &dyn HttpClient, state: &Value, value: &mut Value) {
        if !core::text(&state["jwt"]).is_some_and(|jwt| endpoint::live_jwt(jwt).is_ok()) {
            return;
        }
        match self.balance(client, state, false).await {
            Ok(gift) => {
                if let Some(gift) = &gift {
                    let exclude = gift
                        .all
                        .iter()
                        .map(|model| readings::gift_id(model))
                        .collect::<Vec<_>>();
                    for item in value["items"]
                        .as_array_mut()
                        .into_iter()
                        .flatten()
                        .filter(|item| item["countsForRouting"] == true)
                    {
                        item["notModels"] = json!(exclude);
                    }
                    value["items"]
                        .as_array_mut()
                        .unwrap()
                        .extend(gift.windows.clone());
                }
                if let Some(cached) = self.routes.lock().await.get_mut(&route_key(state)) {
                    cached.gift = gift;
                    cached.at = core::now();
                    cached.ttl = cached
                        .gift
                        .as_ref()
                        .map(|gift| gift.ttl)
                        .unwrap_or(600_000)
                        .min(600_000);
                }
            }
            Err(_) => {
                if let Some(cached) = self.routes.lock().await.get_mut(&route_key(state)) {
                    cached.gift = None;
                    cached.at = core::now();
                    cached.ttl = 60_000;
                }
            }
        }
    }

    async fn claim_hint(&self, client: &dyn HttpClient, state: &Value) -> Option<Value> {
        let jwt = core::text(&state["jwt"]).filter(|jwt| endpoint::live_jwt(jwt).is_ok())?;
        let preview = self
            .call(
                client,
                state,
                HttpMethod::Get,
                &format!(
                    "{}/api/v1/zcode-plan/billing/preview?app_version={}&platform={}",
                    endpoint::ZCODE,
                    endpoint::VERSION,
                    endpoint::platform()
                ),
                Some(&format!("Bearer {jwt}")),
                BTreeMap::new(),
            )
            .await
            .ok()?;
        let plans = preview["plans"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|plan| core::text(&plan["plan_id"]).is_some())
            .collect::<Vec<_>>();
        if plans.is_empty() {
            return None;
        }
        Some(
            json!({"label":"Gift plans to claim in the ZCode app","display":format!("{} · {}", plans.len(), plans.iter().find_map(|plan| core::text(&plan["name"])).unwrap_or("ZCode gift plan")),"countsForRouting":false}),
        )
    }

    async fn usage_card(
        &self,
        client: &dyn HttpClient,
        state: &Value,
        context: &CallContext,
        route: &Route,
    ) -> PluginResult<Value> {
        let mut value = if route.start {
            let gift = self.balance(client, state, true).await?.ok_or_else(|| {
                PluginError::transport("ZCode's Start Plan has ended or was never started.")
            })?;
            json!({"available":true,"account":{"id":context.account},"plan":gift.plan,"until":gift.until.map(core::iso),"renew":"off","items":gift.windows})
        } else {
            self.coding_usage(client, state, &context.account).await?
        };
        if !route.start {
            self.include_gifts(client, state, &mut value).await;
        }
        value["items"]
            .as_array_mut()
            .unwrap()
            .sort_by_key(|item| item["label"] == "MCP · Month");
        if let Some(hint) = self.claim_hint(client, state).await {
            value["items"].as_array_mut().unwrap().push(hint);
        }
        if !route.start {
            let display = match core::text(&state["jwt"]) {
                None if context
                    .credentials
                    .as_ref()
                    .is_some_and(|stored| stored["type"] == "api") =>
                {
                    Some("not read with an API key: add the account with a ZCode sign-in")
                }
                None => Some("this sign-in has no ZCode session: sign in again"),
                Some(jwt) if endpoint::live_jwt(jwt).is_err() => {
                    Some("ZCode's sign-in has expired: sign in again")
                }
                _ => None,
            };
            if let Some(display) = display {
                value["items"].as_array_mut().unwrap().push(json!({"label":"ZCode gift plans (-Trial models) need ZCode's sign-in","display":display,"countsForRouting":false}));
            }
        }
        Ok(value)
    }
}
#[async_trait]
impl ProviderPlugin for Zcode {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        self.core.initialize(params)
    }
    async fn auth_methods(&self, _: CallContext) -> PluginResult<Value> {
        Ok(self.core.config()?.manifest["contributes"]["provider"]["authMethods"].clone())
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        let method = core::required(&params, "authMethodID")?;
        let client = self.core.params_client(&params)?;
        if method == "source" {
            let inputs = core::inputs(&params).clone();
            let device = self.device.clone();
            let source = inputs.clone();
            let state = tokio::task::spawn_blocking(move || auth::source(&source, &device))
                .await
                .map_err(|_| {
                    PluginError::authentication("The selected ZCode source could not be read.")
                })??;
            let mut credentials = json!({"provider":"zcode","type":"oauth","authMethod":"source","source":inputs,"uid":state["uid"],"accountId":state["uid"],"label":"ZCode selected account","site":state["site"],"org":state["org"],"project":state["project"],"device":state["device"],"expires":0});
            let live = self.state(&credentials, client.as_ref()).await?;
            if core::text(&live["key"]).is_some() {
                self.coding_usage(client.as_ref(), &live, "source").await?;
            } else {
                self.balance(client.as_ref(), &live, true)
                    .await?
                    .ok_or_else(|| {
                        PluginError::authentication(
                            "The selected ZCode account has no active Start Plan.",
                        )
                    })?;
            }
            credentials["plan"] = live["plan"].clone();
            return Ok(json!({"ok":true,"credentials":credentials}));
        }
        if let Some(selected) = method.strip_prefix("apiKey-") {
            let selected = Site::parse(selected)?;
            let key = core::required(core::inputs(&params), "key")?;
            let mut state = json!({"site":selected.id(),"base":selected.base(),"key":key,"device":device::key_id(key)});
            let usage = self.coding_usage(client.as_ref(), &state, "new").await?;
            state["plan"] = usage["plan"].clone();
            return Ok(
                json!({"ok":true,"credentials":{"provider":"zcode","type":"api","authMethod":method,"key":key,"expires":0,"uid":core::fingerprint(&json!(key)),"accountId":core::fingerprint(&json!([selected.id(),key])),"label":format!("{} Coding Plan",selected.id()),"site":selected.id(),"state":state}}),
            );
        }
        let on_team = method.ends_with("-team");
        let selected = match method {
            "zai" | "zai-team" => Site::Zai,
            "bigmodel" | "bigmodel-team" => Site::BigModel,
            _ => return Err(PluginError::unsupported(method)),
        };
        let mut random = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut random);
        let poll = format!("Bearer {}", hex::encode(random));
        let device = uuid::Uuid::new_v4().to_string();
        let flow = endpoint::call(
            client.as_ref(),
            HttpMethod::Post,
            &format!("{}/api/v1/oauth/cli/init", endpoint::ZCODE),
            Some(&poll),
            Some(&json!({"provider":selected.id()})),
            BTreeMap::new(),
            &device,
        )
        .await?;
        let mut url = url::Url::parse(core::required(&flow, "authorize_url")?)
            .map_err(|_| PluginError::authentication("ZCode returned an invalid sign-in page."))?;
        if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
            return Err(PluginError::authentication(
                "ZCode requires an HTTPS sign-in page.",
            ));
        }
        let mut back = url::Url::parse(&format!("{}/app/oauth/login", endpoint::ZCODE)).unwrap();
        back.query_pairs_mut()
            .append_pair("redirect", "zcode://oauth/callback")
            .append_pair("app_version", endpoint::VERSION);
        let redirect_key = if selected == Site::BigModel {
            "redirect"
        } else {
            "redirect_uri"
        };
        let query = url
            .query_pairs()
            .filter(|(key, _)| key != redirect_key)
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect::<Vec<_>>();
        url.set_query(None);
        url.query_pairs_mut()
            .extend_pairs(query)
            .append_pair(redirect_key, back.as_ref());
        let until = readings::number(&flow["expires_at"])
            .filter(|n| *n > 0.)
            .map(|n| (n * 1000.) as u64)
            .unwrap_or(core::now() + 300_000);
        if until <= core::now() {
            return Err(PluginError::authentication(
                "ZCode returned an expired sign-in flow.",
            ));
        }
        let session=self.core.begin(json!({"method":method,"site":selected.id(),"team":on_team,"device":device,"flow":core::required(&flow,"flow_id")?,"poll":poll,"interval":((readings::number(&flow["poll_interval_sec"]).unwrap_or(1.)*1000.)as u64).clamp(1000,30000)}),until).await?;
        Ok(
            json!({"session":session,"url":url.to_string(),"method":"auto","instructions":"Sign in to Z.ai or BigModel in the browser. This completes automatically."}),
        )
    }
    async fn callback(&self, params: Value) -> PluginResult<Value> {
        let credentials =
            tokio::time::timeout(Duration::from_millis(1_200_000), self.complete(&params))
                .await
                .map_err(|_| {
                    PluginError::authentication("The ZCode sign-in timed out. Start again.")
                })??;
        Ok(json!({"ok":true,"credentials":credentials}))
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let client = self.core.params_client(&params)?;
        let state = self
            .state(
                params
                    .get("credentials")
                    .ok_or_else(|| PluginError::authentication("Connect a ZCode account."))?,
                client.as_ref(),
            )
            .await?;
        if core::text(&state["key"]).is_none() {
            endpoint::live_jwt(core::required(&state, "jwt")?)?;
        }
        Ok(json!({"changed":false}))
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        let client = self.core.client(context.network_proxy.as_ref())?;
        let state = if let Some(stored) = &context.credentials {
            Some(self.selected_state(stored).await?)
        } else {
            None
        };
        let route = if let Some(state) = &state {
            self.route(client.as_ref(), state).await
        } else {
            Route {
                start: false,
                gift: None,
                at: 0,
                ttl: 0,
            }
        };
        let selected = state.as_ref().map(site).transpose()?.unwrap_or(Site::Zai);
        let (config, source) = self
            .config(client.as_ref(), context.credentials.as_ref())
            .await;
        let mut rows = config
            .as_ref()
            .map(|config| catalog::rules(config, selected, route.start))
            .unwrap_or_default();
        let source = if rows.is_empty() {
            rows = catalog::presets(route.start);
            "preset"
        } else {
            source
        };
        let mut details = rows.clone();
        if let Some(config) = &config {
            details.extend(catalog::rules(config, selected, true));
            details.extend(catalog::rules(config, Site::Zai, false));
        }
        details.extend(catalog::presets(false));
        let mut models = rows
            .into_iter()
            .map(|row| catalog::model(&row, selected, route.start, source))
            .collect::<Vec<_>>();
        let gift = if route.start {
            if let Some(state) = &state {
                self.balance(client.as_ref(), state, true)
                    .await
                    .ok()
                    .flatten()
            } else {
                None
            }
        } else {
            route.gift
        };
        if let Some(gift) = gift {
            for raw in gift.all {
                let about = details
                    .iter()
                    .find(|row| {
                        row["id"]
                            .as_str()
                            .is_some_and(|id| id.eq_ignore_ascii_case(&raw))
                    })
                    .cloned()
                    .unwrap_or_else(|| catalog::about(&raw));
                if route.start
                    && !models.iter().any(|model| {
                        model["id"]
                            .as_str()
                            .is_some_and(|id| id.eq_ignore_ascii_case(&raw))
                    })
                {
                    models.push(catalog::model(&about, selected, true, "remote"));
                }
                let mut trial = about;
                trial["id"] = json!(readings::gift_id(&raw));
                if !models.iter().any(|model| model["id"] == trial["id"]) {
                    models.push(catalog::model(&trial, selected, route.start, "remote"));
                }
            }
        }
        Ok(core::listing(
            models,
            "anthropic-messages",
            &format!(
                "{}/v1",
                if route.start {
                    endpoint::START
                } else {
                    selected.base()
                }
            ),
            source,
        ))
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let client = self.core.client(context.network_proxy.as_ref())?;
        let stored = context
            .credentials
            .as_ref()
            .ok_or_else(|| PluginError::authentication("Connect ZCode to read usage."))?;
        let state = self.state(stored, client.as_ref()).await?;
        let route = self.route(client.as_ref(), &state).await;
        let result = self
            .usage_card(client.as_ref(), &state, &context, &route)
            .await;
        Ok(match result {
            Ok(value) => value,
            Err(error) => {
                json!({"available":false,"account":{"id":context.account},"plan":state["plan"],"items":[],"reason":error.message})
            }
        })
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        let url = url::Url::parse(&request.url)
            .map_err(|_| PluginError::invalid("Invalid ZCode request address."))?;
        if request.method != "POST"
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || ![
                format!("{}/v1/messages", Site::Zai.base()),
                format!("{}/v1/messages", Site::BigModel.base()),
                format!("{}/v1/messages", endpoint::START),
                format!("{}/v1/messages/count_tokens", Site::Zai.base()),
                format!("{}/v1/messages/count_tokens", Site::BigModel.base()),
            ]
            .contains(&request.url)
        {
            return Err(PluginError::invalid(
                "This request is outside ZCode's Messages endpoint.",
            ));
        }
        let path = if url.path().ends_with("/v1/messages/count_tokens") {
            "/v1/messages/count_tokens"
        } else {
            "/v1/messages"
        };
        let client = self.core.client(request.network_proxy.as_ref())?;
        let state = self
            .state(
                request
                    .credentials
                    .as_ref()
                    .ok_or_else(|| PluginError::authentication("Connect a ZCode account."))?,
                client.as_ref(),
            )
            .await?;
        let route = self.route(client.as_ref(), &state).await;
        let mut bytes = request.decode_body()?;
        let mut body = bytes
            .as_deref()
            .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok());
        let renamed = body.as_mut().is_some_and(|body| {
            tool_search::rename_request(body, bytes.as_deref().unwrap_or_default())
        });
        let mut changed = renamed;
        let model = body
            .as_ref()
            .and_then(|body| body["model"].as_str())
            .unwrap_or("")
            .to_owned();
        let forced = model.to_lowercase().ends_with("-trial");
        let start = route.start || forced;
        let mut raw_model = model.clone();
        if forced {
            let gift = match route.gift {
                Some(gift) => Some(gift),
                None => self
                    .balance(client.as_ref(), &state, route.start)
                    .await
                    .ok()
                    .flatten(),
            };
            let raw = gift.as_ref().and_then(|gift| {
                gift.all
                    .iter()
                    .find(|raw| readings::gift_id(raw).eq_ignore_ascii_case(&model))
            });
            let available = raw.is_some_and(|raw| {
                gift.as_ref()
                    .unwrap()
                    .live
                    .iter()
                    .any(|live| live.eq_ignore_ascii_case(raw))
            }) && core::text(&state["jwt"])
                .is_some_and(|jwt| endpoint::live_jwt(jwt).is_ok());
            let blocked = self
                .blocked
                .lock()
                .await
                .get(&blocked_key(
                    &state,
                    raw.map(String::as_str).unwrap_or(&model),
                ))
                .is_some_and(|until| *until > core::now());
            if !available || blocked {
                let status = if raw.is_some() { 429 } else { 400 };
                return Ok(FetchResponse::bytes(status,BTreeMap::from([("content-type".into(),"application/json".into())]),serde_json::to_vec(&json!({"type":"error","error":{"type":if status==429{"rate_limit_error"}else{"invalid_request_error"},"message":"This ZCode Trial model has no active usable gift quota. Refresh usage or choose the Coding Plan model."}})).unwrap()));
            }
            raw_model = raw.unwrap().to_owned();
            let body = body.as_mut().unwrap();
            body["model"] = json!(raw_model);
            changed = true;
        }
        let selected = site(&state)?;
        let (base, headers) = if start {
            if let Some(body) = body.as_mut().filter(|body| body["messages"].is_array()) {
                *body = start::dress(
                    body.take(),
                    selected,
                    core::required(&state, "device")?,
                    request.metadata.get("workingDirectory").map(String::as_str),
                    request
                        .metadata
                        .get("isGitRepository")
                        .and_then(|value| value.parse::<bool>().ok()),
                )?;
                changed = true;
            }
            (
                endpoint::START,
                start::headers(core::required(&state, "jwt")?)?,
            )
        } else {
            let key = core::required(&state, "key")?;
            if path == "/v1/messages"
                && (renamed
                    || bytes
                        .as_deref()
                        .is_some_and(|bytes| bytes.windows(8).any(|part| part == b"messages")))
            {
                if let Some(body) = body.as_mut() {
                    changed |= device::mark_coding_body(body, state["device"].as_str().unwrap());
                }
            }
            let mut headers = BTreeMap::from([
                ("authorization".into(), format!("Bearer {key}")),
                ("x-api-key".into(), key.into()),
                ("content-type".into(), "application/json".into()),
                ("anthropic-version".into(), "2023-06-01".into()),
            ]);
            for (key, value) in &request.headers {
                if ["content-type", "anthropic-version", "anthropic-beta"]
                    .contains(&key.to_lowercase().as_str())
                {
                    headers.insert(key.to_lowercase(), value.clone());
                }
            }
            (selected.base(), headers)
        };
        if changed {
            bytes = body.map(|body| serde_json::to_vec(&body).unwrap());
        }
        let response = tokio::time::timeout(
            Duration::from_secs(60),
            client.stream(HttpRequest {
                method: HttpMethod::Post,
                url: format!("{base}{path}"),
                headers,
                body: bytes,
            }),
        )
        .await
        .map_err(|_| PluginError::transport("ZCode did not answer in time."))?
        .map_err(|_| PluginError::transport("ZCode could not be reached."))?;
        let status = response.status;
        let mut headers = clean(&response.headers);
        let json_response = headers.iter().any(|(key, value)| {
            key.eq_ignore_ascii_case("content-type") && value.contains("application/json")
        });
        let mut restore_search = renamed;
        let response = if start && (status >= 400 || forced && json_response) {
            let bytes = core::collect(response, 16 * 1024 * 1024).await?;
            if start::refused(status, &bytes) {
                headers.insert("content-type".into(), "application/json".into());
                return Ok(FetchResponse::bytes(
                    status,
                    headers,
                    serde_json::to_vec(&start::blocked(status)).unwrap(),
                ));
            }
            let spent = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .is_some_and(|value| start::spent(&value, status))
                || status == 429;
            if forced && spent {
                let mut blocked = self.blocked.lock().await;
                blocked.retain(|_, until| *until > core::now());
                if blocked.len() >= 4096 {
                    blocked.clear();
                }
                blocked.insert(blocked_key(&state, &raw_model), core::now() + 60_000);
            }
            let status = if forced && spent && status < 400 {
                restore_search = false;
                429
            } else {
                status
            };
            FetchResponse::bytes(status, headers, bytes)
        } else {
            let body = futures::stream::unfold(response.body, |mut body| async move {
                match tokio::time::timeout(Duration::from_secs(180), body.next()).await {
                    Ok(Some(Ok(bytes))) => Some((Ok(bytes.to_vec()), body)),
                    Ok(None) => None,
                    _ => Some((
                        Err(PluginError::transport("ZCode's response stopped.")),
                        Box::pin(futures::stream::empty()) as nora_ai::HttpByteStream,
                    )),
                }
            });
            FetchResponse {
                status,
                headers,
                body: Box::pin(body),
            }
        };
        Ok(if restore_search {
            tool_search::restore_response(response)
        } else {
            response
        })
    }
}
#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
