use crate::{
    readings,
    signing::{self, Site},
};
use async_trait::async_trait;
use base64::Engine;
use futures::StreamExt;
use nora_ai::{HttpClient, HttpMethod, HttpStreamingResponse};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, PluginError, PluginResult,
    ProviderPlugin,
};
use nora_provider_subscription_core::{self as core, Core};
use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::Duration;
const CLIENT: &str = "mcode-public";
const DEVICE: &str = "urn:ietf:params:oauth:grant-type:device_code";
pub struct MiniMaxCode {
    core: Core,
}
impl MiniMaxCode {
    pub fn new(id: &'static str) -> Self {
        Self {
            core: Core::new(id),
        }
    }
    fn site(&self) -> PluginResult<Site> {
        Site::for_id(self.core.id)
    }
    async fn live(&self, old: &Value, client: &dyn HttpClient, force: bool) -> PluginResult<Value> {
        let _guard = self.core.refresh_lock.lock().await;
        let current = self.core.latest(old).await?;
        if current["type"] != "oauth" || current["site"] != self.core.id {
            return Err(PluginError::authentication(
                "Connect this MiniMax Code account again.",
            ));
        }
        core::bearer(&current)?;
        let expiry = current["expires"].as_u64().unwrap_or(0);
        let newer = current["access"] != old["access"];
        if (!force || newer) && (expiry == 0 || expiry > core::now() + 120_000) {
            return Ok(current);
        }
        let refresh = core::text(&current["refresh"]).ok_or_else(|| {
            PluginError::authentication(
                "This MiniMax Code account can no longer refresh. Sign in again.",
            )
        })?;
        let fields = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
            ("client_id", CLIENT),
            ("scope", "agent.default"),
            ("audience", "agent-backend"),
        ];
        let outcome = core::exchange(
            client,
            core::form_request(format!("{}/oauth2/token", self.site()?.account), &fields),
        )
        .await;
        let (status, value) = match outcome {
            Ok(answer) => answer,
            Err(_error) if !force && expiry > core::now() => return Ok(current),
            Err(error) => return Err(error),
        };
        if status == 400 && value["error"] == "invalid_grant" {
            return Err(PluginError::authentication(
                "MiniMax Code refused the refresh token. Sign in again.",
            ));
        }
        if !(200..300).contains(&status) || core::text(&value["error"]).is_some() {
            if !force && expiry > core::now() {
                return Ok(current);
            }
            return Err(PluginError::transport(format!(
                "MiniMax Code token renewal returned HTTP {status}; the account was kept."
            )));
        }
        let mut next = current.clone();
        let token = token(&value, refresh)?;
        for (key, value) in token.as_object().unwrap() {
            next[key] = value.clone();
        }
        self.core.persist_rotation(&current, next).await
    }
    async fn account(
        &self,
        client: &dyn HttpClient,
        access: &str,
        user: &str,
        path: &str,
        body: Option<&Value>,
        origin: &str,
    ) -> PluginResult<Value> {
        let request = signing::signed(
            self.site()?,
            path,
            access,
            user,
            body,
            origin,
            core::now(),
            chrono::Local::now().offset().local_minus_utc(),
            if cfg!(target_os = "macos") {
                "darwin"
            } else {
                std::env::consts::OS
            },
        )?;
        let value = core::checked(client, request).await?;
        if value.pointer("/statusInfo/code").and_then(Value::as_i64) == Some(1000048) {
            return Err(PluginError::authentication(
                "MiniMax Code refused this account. Sign in again.",
            ));
        }
        if value
            .pointer("/statusInfo/code")
            .and_then(Value::as_i64)
            .is_some_and(|n| n != 0)
            || value
                .pointer("/base_resp/status_code")
                .and_then(Value::as_i64)
                .is_some_and(|n| n != 0)
        {
            return Err(PluginError::transport(
                "MiniMax Code refused the account information request.",
            ));
        }
        Ok(value)
    }
    async fn identify(&self, client: &dyn HttpClient, access: &str) -> PluginResult<Value> {
        let value = self
            .account(
                client,
                access,
                "",
                "/v1/api/user/info",
                None,
                self.site()?.agent,
            )
            .await?;
        let user = value
            .pointer("/data/userInfo")
            .or_else(|| value.pointer("/data/user_info"))
            .or_else(|| value.get("userInfo"))
            .or_else(|| value.get("user_info"))
            .ok_or_else(|| PluginError::transport("MiniMax Code returned no account profile."))?;
        let id = core::text(&user["realUserID"])
            .or_else(|| core::text(&user["real_user_id"]))
            .ok_or_else(|| PluginError::transport("MiniMax Code returned no account identity."))?;
        Ok(
            json!({"realUserID":id,"email":core::text(&user["userEmail"]).or_else(||core::text(&user["email"])).or_else(||core::text(&user["userMail"])).or_else(||core::text(&user["user_email"])),"name":core::text(&user["name"]).or_else(||core::text(&user["userName"])).or_else(||core::text(&user["user_name"]))}),
        )
    }
    async fn membership(
        &self,
        client: &dyn HttpClient,
        credentials: &Value,
        user: &str,
    ) -> PluginResult<Value> {
        let site = self.site()?;
        let access = core::bearer(credentials)?;
        let response = self
            .account(
                client,
                access,
                user,
                "/matrix/api/v1/user/get_user_extra_info",
                Some(&json!({})),
                site.agent,
            )
            .await;
        let workspaces = match response {
            Ok(value) => value,
            Err(error) if error.code == "authentication_required" => return Err(error),
            Err(_) => {
                return self
                    .account(
                        client,
                        access,
                        user,
                        "/matrix/api/v1/commerce/get_membership_info",
                        Some(&json!({})),
                        site.agent,
                    )
                    .await
                    .map(|value| readings::membership(&value))
            }
        };
        let Some(workspace) = workspaces
            .get("workspaces")
            .or_else(|| workspaces.pointer("/data/workspaces"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|w| w["workspace_type"].as_i64() == Some(0) || w["workspace_type"] == "0")
        else {
            return Ok(json!({}));
        };
        let mut out = readings::membership(workspace);
        let Some(id) = workspace
            .get("workspace_id")
            .filter(|id| id.is_string() || id.is_number())
        else {
            return Ok(out);
        };
        match self
            .account(
                client,
                access,
                user,
                "/matrix/api/v1/commerce/get_membership_info",
                Some(&json!({"workspace_id":id})),
                site.agent,
            )
            .await
        {
            Ok(value) => {
                let fetched = readings::membership(&value);
                for (key, value) in fetched.as_object().unwrap() {
                    if !value.is_null() && !(key == "opGroupId" && !out[key].is_null()) {
                        if key == "hasTokenPlan" && out[key] == true {
                            continue;
                        }
                        out[key] = value.clone();
                    }
                }
            }
            Err(error) if error.code == "authentication_required" => return Err(error),
            Err(_) => {}
        }
        Ok(out)
    }
    async fn send(
        &self,
        client: &dyn HttpClient,
        request: &FetchRequest,
        credentials: &Value,
    ) -> PluginResult<HttpStreamingResponse> {
        let mut outgoing = core::json_request(
            HttpMethod::Post,
            &request.url,
            None,
            Some(core::bearer(credentials)?),
        )?
        .with_header("content-type", "application/json")
        .with_header("x-api-key", "sk-xxx")
        .with_header("user-agent", "MiniMaxAgent")
        .with_header("x-mavis-agent-id", "main")
        .with_header(
            "x-mavis-timezone-offset",
            chrono::Local::now().offset().local_minus_utc().to_string(),
        )
        .with_header(
            "x-mavis-session-id",
            request
                .metadata
                .get("sessionID")
                .cloned()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        );
        outgoing.body = request.decode_body()?;
        for (key, value) in &request.headers {
            if ["accept", "anthropic-beta", "anthropic-version"]
                .contains(&key.to_ascii_lowercase().as_str())
            {
                outgoing.headers.insert(key.clone(), value.clone());
            }
        }
        outgoing
            .headers
            .entry("anthropic-version".into())
            .or_insert_with(|| "2023-06-01".into());
        tokio::time::timeout(Duration::from_secs(60), client.stream(outgoing))
            .await
            .map_err(|_| PluginError::transport("MiniMax Code did not answer in time."))?
            .map_err(|_| PluginError::transport("MiniMax Code could not be reached."))
    }
}
fn token(value: &Value, refresh: &str) -> PluginResult<Value> {
    let access = core::required(value, "access_token")?;
    let seconds = value["expires_in"]
        .as_u64()
        .filter(|n| *n > 0 && *n <= 365 * 86400)
        .ok_or_else(|| {
            PluginError::transport("MiniMax Code returned an invalid token lifetime.")
        })?;
    Ok(
        json!({"access":access,"refresh":core::text(&value["refresh_token"]).unwrap_or(refresh),"expires":core::now().saturating_add(seconds*1000)}),
    )
}
#[async_trait]
impl ProviderPlugin for MiniMaxCode {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        self.site()?;
        let mut result = self.core.initialize(params)?;
        result["capabilities"]["sdkFeatures"] = json!({"accountAutomation":1});
        Ok(result)
    }
    async fn auth_methods(&self, _: CallContext) -> PluginResult<Value> {
        Ok(self.core.config()?.manifest["contributes"]["provider"]["authMethods"].clone())
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        if core::required(&params, "authMethodID")? != "oauth" {
            return Err(PluginError::unsupported("this MiniMax Code sign-in method"));
        }
        let client = self.core.params_client(&params)?;
        let site = self.site()?;
        let mut random = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut random);
        let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random);
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        let value = core::checked(
            client.as_ref(),
            core::form_request(
                format!("{}/oauth2/device/code", site.account),
                &[
                    ("client_id", CLIENT),
                    ("scope", "agent.default"),
                    ("audience", "agent-backend"),
                    ("code_challenge", &challenge),
                    ("code_challenge_method", "S256"),
                ],
            ),
        )
        .await?;
        let code = core::required(&value, "user_code")?;
        let by_user =
            core::text(&value["device_code"]).is_none() && value["expired_in"].is_number();
        let poll = if by_user {
            json!({"user_code":code})
        } else {
            json!({"device_code":core::required(&value,"device_code")?})
        };
        let url = core::text(&value["verification_uri_complete"])
            .or_else(|| core::text(&value["verification_uri"]))
            .or_else(|| core::text(&value["verification_url"]))
            .ok_or_else(|| PluginError::transport("MiniMax Code returned no approval address."))?;
        let parsed = url::Url::parse(url).map_err(|_| {
            PluginError::transport("MiniMax Code returned an invalid approval address.")
        })?;
        if parsed.scheme() != "https"
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err(PluginError::transport(
                "MiniMax Code returned an unsafe approval address.",
            ));
        }
        let expires = value["expires_in"]
            .as_u64()
            .map(|n| core::now() + n.min(900) * 1000)
            .or_else(|| {
                value["expired_in"].as_u64().map(|n| {
                    if n > 1_000_000_000_000 {
                        n
                    } else {
                        core::now() + n.min(900) * 1000
                    }
                })
            })
            .unwrap_or_else(|| core::now() + 600_000);
        let interval = value["interval"].as_u64().unwrap_or(5);
        let interval = if by_user {
            interval.clamp(100, 30_000)
        } else {
            interval.clamp(1, 30) * 1000
        };
        let session = self
            .core
            .begin(
                json!({"verifier":verifier,"poll":poll,"interval":interval}),
                expires,
            )
            .await?;
        Ok(
            json!({"session":session,"url":url,"method":"auto","instructions":format!("Sign in to MiniMax Code and confirm code {code}. This finishes here automatically.")}),
        )
    }
    async fn callback(&self, params: Value) -> PluginResult<Value> {
        let pending = self.core.take_pending(&params).await?;
        let client = self.core.params_client(&params)?;
        let site = self.site()?;
        let mut wait = pending["interval"].as_u64().unwrap_or(5000);
        while core::now() < pending["until"].as_u64().unwrap_or(0) {
            tokio::time::sleep(Duration::from_millis(wait)).await;
            let verifier = core::required(&pending, "verifier")?;
            let mut fields = vec![
                ("grant_type", DEVICE),
                ("client_id", CLIENT),
                ("code_verifier", verifier),
            ];
            if let Some(code) = core::text(&pending["poll"]["device_code"]) {
                fields.push(("device_code", code));
            } else {
                fields.push(("user_code", core::required(&pending["poll"], "user_code")?));
            }
            let (status, value) = core::exchange(
                client.as_ref(),
                core::form_request(format!("{}/oauth2/token", site.account), &fields),
            )
            .await?;
            let state = core::text(&value["error"])
                .or_else(|| core::text(&value["status"]))
                .unwrap_or("");
            if ["authorization_pending", "pending"].contains(&state) {
                continue;
            }
            if state == "slow_down" {
                wait = (wait + 5000).min(30_000);
                continue;
            }
            if !(200..300).contains(&status)
                || ["denied", "access_denied", "expired", "expired_token"].contains(&state)
            {
                return Err(PluginError::authentication(
                    "MiniMax Code sign-in was refused or expired.",
                ));
            }
            let mut credentials = token(&value, "")?;
            credentials["type"] = json!("oauth");
            credentials["authMethod"] = json!("oauth");
            credentials["site"] = json!(site.id);
            let identity = self
                .identify(client.as_ref(), core::bearer(&credentials)?)
                .await?;
            for (key, value) in identity.as_object().unwrap() {
                credentials[key] = value.clone();
            }
            credentials["accountId"] = identity["realUserID"].clone();
            credentials["label"] = core::text(&identity["email"])
                .or_else(|| core::text(&identity["name"]))
                .map(|v| json!(v))
                .unwrap_or_else(|| json!("MiniMax Code account"));
            crate::identity::assign(site, &mut credentials)?;
            return Ok(json!({"ok":true,"credentials":credentials}));
        }
        Err(PluginError::authentication(
            "The MiniMax Code sign-in code expired.",
        ))
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let client = self.core.params_client(&params)?;
        let credentials = params.get("credentials").ok_or_else(|| {
            PluginError::authentication("Connect this MiniMax Code account again.")
        })?;
        self.live(credentials, client.as_ref(), params["force"] == true)
            .await?;
        Ok(json!({"changed":false}))
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        let site = self.site()?;
        let Some(old) = &context.credentials else {
            return Ok(core::listing(
                readings::presets(site),
                "anthropic-messages",
                &site.base(),
                "preset",
            ));
        };
        let client = self.core.client(context.network_proxy.as_ref())?;
        let credentials = self.live(old, client.as_ref(), false).await?;
        let url = format!(
            "{}/mavis/api/v1/models?region={}&buildEnv=prod",
            site.llm, site.region
        );
        let (status, value) = core::exchange(
            client.as_ref(),
            core::json_request(
                HttpMethod::Get,
                &url,
                None,
                Some(core::bearer(&credentials)?),
            )?,
        )
        .await?;
        let value = if status == 401 || status == 403 {
            core::checked(
                client.as_ref(),
                core::json_request(HttpMethod::Get, &url, None, None)?,
            )
            .await?
        } else if (200..300).contains(&status) {
            value
        } else {
            return Err(PluginError::transport(format!(
                "MiniMax Code model discovery returned HTTP {status}."
            )));
        };
        Ok(core::listing(
            readings::models(&value, site, "remote")?,
            "anthropic-messages",
            &site.base(),
            "remote",
        ))
    }
    async fn accounts(&self, context: CallContext) -> PluginResult<Value> {
        let Some(mut credentials) = context.credentials else {
            return Ok(json!([]));
        };
        let previous = credentials.clone();
        crate::identity::assign(self.site()?, &mut credentials)?;
        let mut result = json!({"accounts":[{"id":context.account,"email":credentials["email"],"name":credentials["name"],"configured":credentials["type"]=="oauth"}]});
        if previous != credentials {
            result["credentials"] = credentials;
        }
        Ok(result)
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let client = self.core.client(context.network_proxy.as_ref())?;
        let old = context.credentials.as_ref().ok_or_else(|| {
            PluginError::authentication("Connect MiniMax Code to read its allowance.")
        })?;
        let credentials = self.live(old, client.as_ref(), false).await?;
        let identity = if core::text(&credentials["realUserID"]).is_some() {
            credentials.clone()
        } else {
            self.identify(client.as_ref(), core::bearer(&credentials)?)
                .await?
        };
        let member = self
            .membership(
                client.as_ref(),
                &credentials,
                core::required(&identity, "realUserID")?,
            )
            .await?;
        let mut items = Vec::new();
        let mut warning = None;
        if member["hasTokenPlan"] == true {
            if let Some(group) = core::text(&member["opGroupId"]) {
                let request = core::json_request(
                    HttpMethod::Get,
                    format!(
                        "{}/v1/api/openplatform/coding_plan/remains",
                        self.site()?.platform
                    ),
                    None,
                    Some(core::bearer(&credentials)?),
                )?
                .with_header("x-group-id", group);
                match core::checked(client.as_ref(), request)
                    .await
                    .and_then(|value| readings::plan_windows(&value))
                {
                    Ok(read) => items = read,
                    Err(error) => warning = Some(error.message),
                }
            }
        }
        Ok(
            json!({"available":true,"account":{"id":context.account,"email":identity["email"],"name":identity["name"],"plan":if member["hasTokenPlan"]==true {format!("M Plan {}",member["tier"].as_str().unwrap_or(""))}else{"Free".into()}},"balance":member["balance"],"until":core::timestamp(&member["expiresAt"]).map(core::iso),"items":items,"error":warning}),
        )
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        let site = self.site()?;
        core::validate_fetch(&request, &site.base(), &["/mavis/api/v1/llm/v1/messages"])?;
        let client = self.core.client(request.network_proxy.as_ref())?;
        let old = request
            .credentials
            .as_ref()
            .ok_or_else(|| PluginError::authentication("Connect a MiniMax Code account."))?;
        let credentials = self.live(old, client.as_ref(), false).await?;
        let mut response = self.send(client.as_ref(), &request, &credentials).await?;
        if response.status == 401 {
            drop(response);
            let next = self.live(old, client.as_ref(), true).await?;
            response = self.send(client.as_ref(), &request, &next).await?;
        }
        let mut headers = response.headers.clone();
        headers.retain(|key, _| {
            ![
                "content-length",
                "content-encoding",
                "transfer-encoding",
                "set-cookie",
                "connection",
            ]
            .contains(&key.to_ascii_lowercase().as_str())
        });
        if matches!(response.status, 402 | 403 | 429) {
            let status = response.status;
            let bytes = core::collect(response, 1024 * 1024).await?;
            let message = String::from_utf8_lossy(&bytes).to_lowercase();
            let quota = [
                "insufficient",
                "balance",
                "credit",
                "quota",
                "exhaust",
                "limit",
                "余额",
                "积分",
                "额度",
                "不足",
                "用完",
                "上限",
            ]
            .iter()
            .any(|term| message.contains(term));
            return Ok(FetchResponse::bytes(
                if quota { 429 } else { status },
                headers,
                bytes,
            ));
        }
        Ok(FetchResponse {
            status: response.status,
            headers,
            body: Box::pin(response.body.map(|chunk| {
                chunk
                    .map(|b| b.to_vec())
                    .map_err(|_| PluginError::transport("MiniMax Code's response stopped."))
            })),
        })
    }
    async fn call(&self, method: &str, params: Value) -> PluginResult<Value> {
        if !matches!(method, "checkin" | "checkinStatus") {
            return Err(PluginError::unsupported(method));
        }
        self.checkin(params, method == "checkinStatus").await
    }
}
impl MiniMaxCode {
    async fn checkin(&self, params: Value, status_only: bool) -> PluginResult<Value> {
        let client = self.core.params_client(&params)?;
        let old = params.get("credentials").ok_or_else(|| {
            PluginError::authentication("Connect MiniMax Code before checking in.")
        })?;
        let credentials = self.live(old, client.as_ref(), false).await?;
        let site = self.site()?;
        let panel = self
            .account(
                client.as_ref(),
                core::bearer(&credentials)?,
                "",
                "/minimax-cloud/api/v1/signin/status",
                None,
                site.llm,
            )
            .await?;
        let days = panel
            .pointer("/data/days")
            .and_then(Value::as_array)
            .filter(|days| days.len() == 7)
            .ok_or_else(|| {
                PluginError::transport("MiniMax Code returned an invalid daily check-in panel.")
            })?;
        let mut sorted: Vec<&Value> = days.iter().collect();
        sorted.sort_by_key(|d| d["day_no"].as_u64().unwrap_or(0));
        if sorted
            .iter()
            .enumerate()
            .any(|(i, d)| d["day_no"].as_u64() != Some(i as u64 + 1))
        {
            return Err(PluginError::transport(
                "MiniMax Code returned an invalid daily check-in panel.",
            ));
        }
        let today: Vec<usize> = sorted
            .iter()
            .enumerate()
            .filter_map(|(i, d)| (d["is_today"] == true).then_some(i))
            .collect();
        if today.len() > 1 || sorted.iter().filter(|d| d["status"] == 2).count() > 1 {
            return Err(PluginError::transport(
                "MiniMax Code returned an ambiguous daily check-in panel.",
            ));
        }
        let Some(&index) = today.first() else {
            return Ok(json!({"outcome":"inactive"}));
        };
        let today = sorted[index];
        let streak = |end: usize| {
            sorted[..end]
                .iter()
                .rev()
                .take_while(|d| d["status"] == 3)
                .count()
        };
        match today["status"].as_u64().unwrap_or(0) {
            3 => Ok(json!({"outcome":"done","credit":today["points"],"streak":streak(index+1)})),
            4 => Ok(json!({"outcome":"ineligible"})),
            2 if status_only => Ok(json!({"outcome":"ready","retrySafe":true})),
            2 => {
                let claim = self
                    .account(
                        client.as_ref(),
                        core::bearer(&credentials)?,
                        "",
                        "/minimax-cloud/api/v1/signin/claim",
                        Some(&json!({})),
                        site.llm,
                    )
                    .await?;
                let result = claim
                    .pointer("/data/claim_result")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| {
                        PluginError::transport("MiniMax Code returned no check-in result.")
                    })?;
                match result {
                    1 | 2 => Ok(
                        json!({"outcome":if result==1{"claimed"}else{"done"},"credit":claim["data"]["points"],"streak":streak(index)+1}),
                    ),
                    _ => Err(PluginError::transport(
                        "MiniMax Code returned an unknown check-in result.",
                    )),
                }
            }
            _ => Ok(json!({"outcome":"inactive"})),
        }
    }
}

#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
