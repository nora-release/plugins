use crate::{models, usage};
use async_trait::async_trait;
use futures::StreamExt;
use nora_ai::{HttpClient, HttpMethod, HttpRequest};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, PluginError, PluginResult,
    ProviderPlugin,
};
use nora_provider_subscription_core::{self as core, Core};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, time::Duration};
pub(crate) const BASE: &str = "https://api.cline.bot/api/v1";
const WORKOS_CLIENT: &str = "client_01K3A541FN8TA3EPPHTD2325AR";
const DEVICE: &str = "https://api.workos.com/user_management/authorize/device";
const TOKEN: &str = "https://api.workos.com/user_management/authenticate";

async fn poll_approval(
    client: &dyn HttpClient,
    device_code: &str,
) -> PluginResult<Option<(u16, Value)>> {
    let request = core::form_request(
        TOKEN,
        &[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device_code),
            ("client_id", WORKOS_CLIENT),
        ],
    );
    let Ok(Ok(response)) =
        tokio::time::timeout(Duration::from_secs(30), client.stream(request)).await
    else {
        return Ok(None);
    };
    let status = response.status;
    if status >= 500 {
        return Ok(None);
    }
    let value = core::collect(response, core::MAX_JSON)
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    match value {
        Some(value) => Ok(Some((status, value))),
        None if (200..300).contains(&status) => Ok(None),
        None => Err(PluginError::authentication(format!(
            "Cline refused the sign-in approval (HTTP {status})."
        ))),
    }
}

pub(crate) fn headers(session: Option<&str>) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::from([
        ("http-referer".into(), "https://cline.bot".into()),
        ("x-title".into(), "Cline".into()),
        ("user-agent".into(), "Cline/3.0.68".into()),
        ("x-is-multiroot".into(), "false".into()),
        ("x-client-type".into(), "cline-cli".into()),
        ("x-client-version".into(), "3.0.68".into()),
        ("x-platform".into(), "cli".into()),
        ("x-platform-version".into(), "3.0.68".into()),
        ("x-core-version".into(), "0.0.90".into()),
    ]);
    if let Some(session) = session.filter(|s| !s.is_empty()) {
        let mut digest = Sha256::new();
        digest.update(b"cline task\0");
        digest.update(session.as_bytes());
        let digest = digest.finalize();
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let h = hex::encode(bytes);
        headers.insert(
            "x-task-id".into(),
            format!(
                "{}-{}-{}-{}-{}",
                &h[..8],
                &h[8..12],
                &h[12..16],
                &h[16..20],
                &h[20..]
            ),
        );
    }
    headers
}
pub struct Cline {
    core: Core,
}
impl Default for Cline {
    fn default() -> Self {
        Self {
            core: Core::new("cline"),
        }
    }
}
fn bearer(value: &Value) -> PluginResult<String> {
    let key = core::bearer(value)?;
    if ["api", "apiKey"].contains(&value["type"].as_str().unwrap_or("")) {
        return Ok(key.to_owned());
    }
    Ok(if key.to_ascii_lowercase().starts_with("workos:") {
        key.to_owned()
    } else {
        format!("workos:{key}")
    })
}
fn parse_auth(value: &Value, refresh: &str) -> PluginResult<Value> {
    let access = core::required(value, "accessToken")?;
    let refresh = core::text(&value["refreshToken"])
        .filter(|s| !s.contains(['\r', '\n', '\0']))
        .unwrap_or(refresh);
    if refresh.is_empty() {
        return Err(PluginError::authentication(
            "Cline did not return a refresh token.",
        ));
    }
    let user = &value["userInfo"];
    let uid = core::text(&user["clineUserId"])
        .or_else(|| core::text(&user["subject"]))
        .unwrap_or("");
    let email = core::text(&user["email"]).unwrap_or("");
    Ok(
        json!({"type":"oauth","authMethod":"oauth","provider":"cline","access":access,"refresh":refresh,"expires":core::timestamp(&value["expiresAt"]).filter(|n|*n>core::now()).unwrap_or_else(||core::now()+3600_000),"uid":uid,"email":email,"name":user["name"],"accountId":if email.is_empty(){uid}else{email},"label":if email.is_empty(){"Cline account"}else{email}}),
    )
}
fn envelope(value: Value) -> PluginResult<Value> {
    if value.get("success").is_some() {
        if value["success"] != true {
            return Err(PluginError::transport("Cline refused the account request."));
        }
        return Ok(value["data"].clone());
    }
    Ok(value)
}
fn api(
    path: &str,
    method: HttpMethod,
    body: Option<&Value>,
    key: Option<&str>,
) -> PluginResult<HttpRequest> {
    let mut request = core::json_request(method, format!("{BASE}{path}"), body, key)?;
    request.headers.extend(headers(None));
    Ok(request)
}
impl Cline {
    pub fn for_id(id: &'static str) -> Self {
        Self {
            core: Core::new(id),
        }
    }
    fn scoped_models(&self, mut models: Vec<Value>) -> Vec<Value> {
        if self.core.id == "clinepass" {
            models.retain(|model| {
                model["id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("cline-pass/"))
            });
        }
        models
    }
    async fn live(&self, old: &Value, client: &dyn HttpClient, force: bool) -> PluginResult<Value> {
        if ["api", "apiKey"].contains(&old["type"].as_str().unwrap_or("")) {
            core::bearer(old)?;
            return Ok(old.clone());
        }
        let _guard = self.core.refresh_lock.lock().await;
        let current = self.core.latest(old).await?;
        if current["provider"] != "cline" || current["type"] != "oauth" {
            return Err(PluginError::authentication("Reconnect this Cline account."));
        }
        let expiry = current["expires"].as_u64().unwrap_or(0);
        let newer = current["access"] != old["access"];
        if (!force || newer) && expiry > core::now() + 300_000 {
            return Ok(current);
        }
        let refresh = core::text(&current["refresh"]).ok_or_else(|| {
            PluginError::authentication("The Cline refresh token is missing. Sign in again.")
        })?;
        let result = core::exchange(
            client,
            api(
                "/auth/refresh",
                HttpMethod::Post,
                Some(&json!({"refreshToken":refresh,"grantType":"refresh_token"})),
                None,
            )?,
        )
        .await;
        let (status, value) = match result {
            Ok(value) => value,
            Err(_) if !force && expiry > core::now() + 30_000 => return Ok(current),
            Err(error) => return Err(error),
        };
        if !(200..300).contains(&status) {
            let message = value.to_string().to_lowercase();
            let expired = matches!(status, 400 | 401 | 403)
                && ["invalid", "expired", "revoked", "unauthorized"]
                    .iter()
                    .any(|w| message.contains(w));
            if expired {
                return Err(PluginError::authentication(
                    "Cline refused the refresh token. Sign in again.",
                ));
            }
            if !force && expiry > core::now() + 30_000 {
                return Ok(current);
            }
            return Err(PluginError::transport(format!(
                "Cline token renewal returned HTTP {status}; the account was kept."
            )));
        }
        let mut next = parse_auth(&envelope(value)?, refresh)?;
        if let (Some(before), Some(after)) = (core::text(&current["uid"]), core::text(&next["uid"]))
        {
            if before != after {
                return Err(PluginError::authentication(
                    "Cline returned a different account while refreshing.",
                ));
            }
        }
        for key in ["uid", "email", "name", "accountId", "label"] {
            if core::text(&next[key]).is_none() || key == "accountId" {
                if !current[key].is_null() {
                    next[key] = current[key].clone();
                }
            }
        }
        self.core.persist_rotation(&current, next).await
    }
    async fn account(&self, client: &dyn HttpClient, path: &str, key: &str) -> PluginResult<Value> {
        envelope(core::checked(client, api(path, HttpMethod::Get, None, Some(key))?).await?)
    }
}
fn pin_body(mut bytes: Vec<u8>, chat: &Value, enabled: bool) -> PluginResult<Vec<u8>> {
    if !enabled || chat.get("providerOptions").is_some() || chat.get("provider").is_some() {
        return Ok(bytes);
    }
    let model = chat["model"].as_str().unwrap_or("");
    if model.starts_with("cline-free/") {
        return Ok(bytes);
    }
    let name = model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_ascii_lowercase();
    let pinned = if name == "glm-5.3-flash" {
        Some(("provider", json!({"only":["z-ai"]})))
    } else if name.starts_with("deepseek") {
        Some(("providerOptions", json!({"gateway":{"only":["deepseek"]}})))
    } else if name.starts_with("glm") {
        Some(("providerOptions", json!({"gateway":{"only":["zai"]}})))
    } else {
        None
    };
    let Some((key, value)) = pinned else {
        return Ok(bytes);
    };
    let end = bytes
        .iter()
        .rposition(|byte| *byte == b'}')
        .ok_or_else(|| PluginError::invalid("The Cline chat request is invalid."))?;
    let head = &bytes[..end];
    let previous = head
        .iter()
        .rfind(|byte| !byte.is_ascii_whitespace())
        .copied()
        .unwrap_or(b'{');
    let field = format!(
        "{}{}:{}",
        if previous == b'{' || previous == b',' {
            ""
        } else {
            ","
        },
        serde_json::to_string(key).unwrap(),
        value
    );
    bytes.splice(end..end, field.bytes());
    Ok(bytes)
}
fn failure(status: u16, value: &Value) -> (u16, String) {
    let message = core::text(&value["error"]["message"])
        .or_else(|| core::text(&value["message"]))
        .unwrap_or("Cline refused the request.");
    let lower = message.to_ascii_lowercase();
    let code = core::text(&value["error"]["code"])
        .or_else(|| core::text(&value["code"]))
        .unwrap_or("");
    if code == "ENTITLEMENT_ERROR"
        || lower.contains("the user is not subscribed to required model plan")
        || lower.contains("no access to clinepass subscription models yet. subscribe to clinepass")
    {
        return (
            403,
            "This model requires a ClinePass subscription the account does not have.".into(),
        );
    }
    if status == 402
        || code == "insufficient_credits"
        || lower.contains("insufficient_credits")
        || (lower.contains("insufficient balance") && lower.contains("cline credits balance"))
    {
        return (
            429,
            "The Cline account is out of credits. Top up at https://app.cline.bot/credits.".into(),
        );
    }
    if status == 401
        || (status == 403
            && [
                "unauthorized",
                "invalid_token",
                "invalid token",
                "sign in",
                "sign-in",
                "not signed in",
                "token expired",
                "token invalid",
                "token revoked",
                "access token expired",
                "refresh token expired",
            ]
            .contains(&lower.trim()))
    {
        return (401, "Cline refused the account's sign-in.".into());
    }
    if status == 429
        || code == "SPEND_LIMIT_EXCEEDED"
        || [
            "rate limit",
            "too many requests",
            "quota exceeded",
            "spend limit",
        ]
        .iter()
        .any(|part| lower.contains(part))
    {
        return (
            429,
            format!(
                "Cline usage limit reached: {}",
                message.chars().take(1000).collect::<String>()
            ),
        );
    }
    (status, message.chars().take(2000).collect())
}
#[async_trait]
impl ProviderPlugin for Cline {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        self.core.initialize(params)
    }
    async fn auth_methods(&self, _: CallContext) -> PluginResult<Value> {
        Ok(self.core.config()?.manifest["authMethods"].clone())
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        let method = core::required(&params, "authMethodID")?;
        if self.core.id == "clinepass" && method != "apiKey" {
            return Err(PluginError::unsupported(method));
        }
        let client = self.core.params_client(&params)?;
        match method {
            "apiKey" => {
                let key = core::required(core::inputs(&params), "apiKey")?;
                self.account(client.as_ref(), "/users/me", key).await?;
                Ok(json!({"ok":true,"credentials":core::saved_api(key)}))
            }
            "oauth" => {
                let value = core::checked(
                    client.as_ref(),
                    core::form_request(DEVICE, &[("client_id", WORKOS_CLIENT)]),
                )
                .await?;
                let code = core::required(&value, "device_code")?;
                let url = core::text(&value["verification_uri_complete"])
                    .or_else(|| core::text(&value["verification_uri"]))
                    .ok_or_else(|| PluginError::transport("Cline returned no approval address."))?;
                let parsed = url::Url::parse(url).map_err(|_| {
                    PluginError::transport("Cline returned an invalid approval address.")
                })?;
                if parsed.scheme() != "https"
                    || !parsed.username().is_empty()
                    || parsed.password().is_some()
                {
                    return Err(PluginError::transport(
                        "Cline returned an unsafe approval address.",
                    ));
                }
                let until =
                    core::now() + value["expires_in"].as_u64().unwrap_or(300).clamp(1, 600) * 1000;
                let interval = value["interval"]
                    .as_f64()
                    .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
                    .map_or(5, |seconds| seconds.floor().clamp(1.0, 30.0) as u64);
                let session = self
                    .core
                    .begin(json!({"device_code":code,"interval":interval}), until)
                    .await?;
                Ok(
                    json!({"session":session,"url":url,"method":"auto","instructions":format!("Approve Cline sign-in in the browser. Code: {}",core::text(&value["user_code"]).unwrap_or(""))}),
                )
            }
            method => Err(PluginError::unsupported(method)),
        }
    }
    async fn callback(&self, params: Value) -> PluginResult<Value> {
        let pending = self.core.take_pending(&params).await?;
        let client = self.core.params_client(&params)?;
        let mut interval = pending["interval"].as_u64().unwrap_or(5);
        while core::now() < pending["until"].as_u64().unwrap_or(0) {
            tokio::time::sleep(Duration::from_secs(interval)).await;
            let Some((status, value)) =
                poll_approval(client.as_ref(), core::required(&pending, "device_code")?).await?
            else {
                continue;
            };
            match value["error"].as_str().unwrap_or("") {
                "authorization_pending" => continue,
                "slow_down" => {
                    interval = (interval + 1).min(60);
                    continue;
                }
                "" => {}
                _ => {
                    return Err(PluginError::authentication(
                        "Cline sign-in was refused or expired.",
                    ))
                }
            }
            if !(200..300).contains(&status) {
                return Err(PluginError::authentication(
                    "Cline refused the sign-in approval.",
                ));
            }
            let access = core::required(&value, "access_token")?;
            let refresh = core::text(&value["refresh_token"]).unwrap_or("");
            let registered = core::checked(
                client.as_ref(),
                api(
                    "/auth/register",
                    HttpMethod::Post,
                    Some(&json!({"accessToken":access,"refreshToken":refresh})),
                    None,
                )?,
            )
            .await?;
            return Ok(
                json!({"ok":true,"credentials":parse_auth(&envelope(registered)?,refresh)?}),
            );
        }
        Err(PluginError::authentication(
            "The Cline sign-in code expired.",
        ))
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let client = self.core.params_client(&params)?;
        let credentials = params
            .get("credentials")
            .ok_or_else(|| PluginError::authentication("Sign in to Cline again."))?;
        self.live(credentials, client.as_ref(), params["force"] == true)
            .await?;
        Ok(json!({"changed":false}))
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        let client = self.core.client(context.network_proxy.as_ref())?;
        match models::discover(client.as_ref()).await {
            Ok(models) => Ok(core::listing(
                self.scoped_models(models),
                "openai-completions",
                BASE,
                "remote",
            )),
            Err(_) if context.credentials.is_none() => Ok(core::listing(
                self.scoped_models(models::presets()),
                "openai-completions",
                BASE,
                "preset",
            )),
            Err(error) => Err(error),
        }
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let client = self.core.client(context.network_proxy.as_ref())?;
        let old = context
            .credentials
            .as_ref()
            .ok_or_else(|| PluginError::authentication("Connect a Cline account to read usage."))?;
        let credentials = self.live(old, client.as_ref(), false).await?;
        let key = bearer(&credentials)?;
        let profile = match self.account(client.as_ref(), "/users/me", &key).await {
            Ok(profile) => profile,
            Err(error) if error.code == "authentication_required" => return Err(error),
            Err(_) => Value::Null,
        };
        let mut ids = Vec::new();
        for value in [
            &profile["clineUserId"],
            &profile["subject"],
            &profile["id"],
            &credentials["uid"],
        ] {
            if let Some(id) = core::text(value) {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        let mut balance = Value::Null;
        let mut balance_error = None;
        for id in ids {
            match self
                .account(
                    client.as_ref(),
                    &format!("/users/{}/balance", core::encode(id)),
                    &key,
                )
                .await
            {
                Ok(value) => {
                    balance = value;
                    break;
                }
                Err(error) => balance_error = Some(error.message),
            }
        }
        let limits = core::exchange(
            client.as_ref(),
            api(
                "/users/me/plan/usage-limits",
                HttpMethod::Get,
                None,
                Some(&key),
            )?,
        )
        .await;
        let (limits, warning) = match limits {
            Ok((status, value)) if (200..300).contains(&status) => match envelope(value) {
                Ok(limits) => (limits, None),
                Err(error) => (Value::Null, Some(error.message)),
            },
            Ok((404, _)) => (Value::Null, None),
            Ok((status, _)) => (
                Value::Null,
                Some(format!(
                    "ClinePass limits could not be read (HTTP {status})."
                )),
            ),
            Err(error) => (Value::Null, Some(error.message)),
        };
        let mut result = usage::parse(&profile, &balance, &limits, &context.account);
        if let Some(warning) = warning {
            result["quotaError"] = json!(warning);
        }
        if result["available"] != true {
            return Err(PluginError::transport(balance_error.unwrap_or_else(|| {
                "Cline returned no credit balance or plan limits.".into()
            })));
        }
        Ok(result)
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        core::validate_fetch(&request, BASE, &["/api/v1/chat/completions"])?;
        let client = self.core.client(request.network_proxy.as_ref())?;
        let old = request
            .credentials
            .as_ref()
            .ok_or_else(|| PluginError::authentication("Connect a Cline account."))?;
        let credentials = self.live(old, client.as_ref(), false).await?;
        let bytes = request
            .decode_body()?
            .ok_or_else(|| PluginError::invalid("The Cline chat request is missing."))?;
        if bytes.len() as u64 > nora_provider_plugin_sdk::MAX_BODY_FILE_BYTES {
            return Err(PluginError::invalid(
                "The Cline request exceeds the 512 MiB gateway limit.",
            ));
        }
        let chat: Value = serde_json::from_slice(&bytes)
            .map_err(|_| PluginError::invalid("The Cline chat request is invalid JSON."))?;
        if !chat.is_object() || !chat["messages"].is_array() {
            return Err(PluginError::invalid(
                "Cline requires a Chat Completions request.",
            ));
        }
        let config = self.core.config()?;
        let pin = config
            .manifest
            .pointer("/config/pinUpstream")
            .is_some_and(|v| v == true || v == "true");
        let body = pin_body(bytes, &chat, pin)?;
        let mut outgoing = core::json_request(
            HttpMethod::Post,
            &request.url,
            None,
            Some(&bearer(&credentials)?),
        )?
        .with_header("content-type", "application/json")
        .with_body(body);
        outgoing.headers.extend(headers(
            request.metadata.get("sessionID").map(String::as_str),
        ));
        if let Some(accept) = request
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("accept"))
            .map(|(_, value)| value)
        {
            outgoing.headers.insert("accept".into(), accept.clone());
        }
        let response = tokio::time::timeout(Duration::from_secs(60), client.stream(outgoing))
            .await
            .map_err(|_| PluginError::transport("Cline did not answer in time."))?
            .map_err(|_| PluginError::transport("Cline could not be reached."))?;
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
        let json_reply = headers.iter().any(|(key, value)| {
            key.eq_ignore_ascii_case("content-type") && value.contains("application/json")
        });
        if status >= 400 {
            let bytes = core::collect(response, 1024 * 1024).await?;
            let value = serde_json::from_slice::<Value>(&bytes)
                .unwrap_or_else(|_| json!({"message":String::from_utf8_lossy(&bytes)}));
            let (status, message) = failure(status, &value);
            return Ok(FetchResponse::bytes(
                status,
                BTreeMap::from([("content-type".into(), "application/json".into())]),
                serde_json::to_vec(
                    &json!({"error":{"type":"cline_error","message":message,"code":status}}),
                )
                .unwrap(),
            ));
        }
        if json_reply {
            let bytes = core::collect(response, core::MAX_JSON).await?;
            let value = serde_json::from_slice::<Value>(&bytes).ok();
            let body = match value {
                Some(value) if value.pointer("/data/choices").is_some_and(Value::is_array) => {
                    serde_json::to_vec(&value["data"]).unwrap()
                }
                _ => bytes,
            };
            return Ok(FetchResponse::bytes(status, headers, body));
        }
        Ok(FetchResponse {
            status,
            headers,
            body: Box::pin(response.body.map(|chunk| {
                chunk
                    .map(|bytes| bytes.to_vec())
                    .map_err(|_| PluginError::transport("Cline's response stopped."))
            })),
        })
    }
}
#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
