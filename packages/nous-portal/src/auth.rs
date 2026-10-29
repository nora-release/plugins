use crate::http;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use futures::{future::BoxFuture, FutureExt};
use nora_ai::{HttpClient, HttpMethod, HttpRequest};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::sync::Mutex;

pub(crate) const PORTAL: &str = "https://portal.nousresearch.com";
pub(crate) const INFERENCE: &str = "https://inference-api.nousresearch.com/v1";
pub(crate) const WELCOME: &str = "https://welcome-api.nousresearch.com/v1";
pub(crate) const CLIENT: &str = "hermes-cli";
pub(crate) const SCOPE: &str = "inference:invoke";
pub(crate) const LEAD: i64 = 120_000;

pub(crate) fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn has_error(body: &Value) -> bool {
    body.get("error").is_some_and(|error| match error {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.is_empty(),
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.),
        _ => true,
    })
}
pub(crate) fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("").trim()
}
pub(crate) fn error(code: &str, expired: bool, status: u16) -> PluginError {
    let suffix = if status == 0 {
        String::new()
    } else {
        format!(" (HTTP {status})")
    };
    PluginError::new(
        if expired {
            "authentication_required"
        } else {
            "upstream_transport_error"
        },
        format!(
            "Nous Portal: {code}{suffix}. {}",
            if expired {
                "Sign in again."
            } else {
                "Credentials kept; retry when the problem is resolved."
            }
        ),
    )
}
pub(crate) fn claims(token: &str) -> Value {
    let parts = token.split('.').collect::<Vec<_>>();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        return Value::Null;
    }
    URL_SAFE_NO_PAD
        .decode(parts[1].trim_end_matches('='))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .filter(Value::is_object)
        .unwrap_or(Value::Null)
}
pub(crate) fn inference_url(value: &str) -> String {
    url::Url::parse(value)
        .ok()
        .filter(|u| {
            u.username().is_empty()
                && u.password().is_none()
                && u.query().is_none()
                && u.fragment().is_none()
        })
        .map(|u| u.as_str().trim_end_matches('/').to_owned())
        .filter(|s| [INFERENCE, WELCOME].contains(&s.as_str()))
        .unwrap_or_else(|| INFERENCE.into())
}
pub(crate) fn verification_url(value: &str) -> PluginResult<String> {
    let url = url::Url::parse(value).map_err(|_| error("invalid_verification_uri", false, 0))?;
    if url.origin().ascii_serialization() != PORTAL
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(error("invalid_verification_uri", false, 0));
    }
    Ok(url.to_string())
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Metadata {
    pub version: u8,
    pub token: String,
    pub scope: String,
    pub token_type: String,
    pub base_url: String,
}
pub(crate) fn metadata(auth: &Value) -> PluginResult<Metadata> {
    let mut m: Metadata = serde_json::from_str(text(auth, "refresh"))
        .map_err(|_| error("unreadable_saved_sign_in", false, 0))?;
    if m.version != 1 || m.token.trim().is_empty() {
        return Err(error("unreadable_saved_sign_in", false, 0));
    }
    m.base_url = inference_url(&m.base_url);
    Ok(m)
}
fn scopes(value: &Value) -> Vec<&str> {
    match value {
        Value::String(s) => s
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|s| !s.is_empty())
            .collect(),
        Value::Array(values) => values.iter().flat_map(scopes).collect(),
        _ => Vec::new(),
    }
}
pub(crate) fn usable(auth: &Value, at: i64, lead: i64) -> bool {
    let Ok(m) = metadata(auth) else {
        return false;
    };
    let c = claims(text(auth, "access"));
    c.is_object()
        && m.token_type.eq_ignore_ascii_case("bearer")
        && scopes(&json!(m.scope))
            .into_iter()
            .chain(scopes(&c["scope"]))
            .chain(scopes(&c["scp"]))
            .any(|s| s == SCOPE)
        && auth["expires"]
            .as_f64()
            .is_some_and(|n| n.is_finite() && n > (at + lead) as f64)
        && c.get("exp").is_none_or(|v| {
            v.as_f64()
                .is_some_and(|n| n.is_finite() && n * 1000. > (at + lead) as f64)
        })
}
pub(crate) fn assert_current(auth: &Value, at: i64, lead: i64) -> PluginResult<()> {
    if auth["type"] != "oauth" {
        return Err(error("not_signed_in", true, 0));
    }
    metadata(auth)?;
    if !usable(auth, at, lead) {
        return Err(error("unusable_inference_jwt", false, 0));
    }
    Ok(())
}
pub(crate) fn token(body: &Value, old: Option<&Value>, started: i64) -> PluginResult<Value> {
    let previous = old.map(metadata).transpose()?;
    let access = text(body, "access_token");
    let c = claims(access);
    let exp_ok = c["exp"].as_f64().is_some_and(|n| (n * 1000.).is_finite());
    let mut expiries = Vec::new();
    if let Some(v) = body.get("expires_in").filter(|v| v.is_number() || !exp_ok) {
        expiries.push(
            v.as_f64()
                .map(|n| started as f64 + n * 1000.)
                .unwrap_or(f64::NAN),
        );
    }
    if let Some(v) = c.get("exp") {
        expiries.push(v.as_f64().map(|n| n * 1000.).unwrap_or(f64::NAN));
    }
    let expires = if expiries.is_empty() {
        started.saturating_add(3600_000)
    } else if expiries.iter().all(|n| n.is_finite()) {
        expiries
            .into_iter()
            .fold(f64::INFINITY, f64::min)
            .max(1.)
            .min(i64::MAX as f64) as i64
    } else {
        1
    };
    let m = Metadata {
        version: 1,
        token: if !text(body, "refresh_token").is_empty() {
            text(body, "refresh_token").into()
        } else {
            previous
                .as_ref()
                .map(|m| m.token.clone())
                .unwrap_or_default()
        },
        scope: body["scope"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                previous
                    .as_ref()
                    .map(|m| m.scope.clone())
                    .unwrap_or_default()
            }),
        token_type: if !text(body, "token_type").is_empty() {
            text(body, "token_type").into()
        } else {
            previous
                .as_ref()
                .map(|m| m.token_type.clone())
                .unwrap_or_else(|| "Bearer".into())
        },
        base_url: inference_url(text(body, "inference_base_url")),
    };
    if m.token.is_empty() {
        return Err(error("missing_refresh_token", false, 0));
    }
    let mut out = old
        .cloned()
        .unwrap_or_else(|| json!({"type":"oauth","authMethod":"oauth","provider":"nous-portal"}));
    out["access"] = json!(access);
    out["refresh"] = json!(serde_json::to_string(&m).unwrap());
    out["expires"] = json!(expires);
    if old.is_none() {
        if let Some(id) = c["sub"].as_str().filter(|s| {
            !s.is_empty() && s.len() <= 512 && s.trim() == *s && !s.chars().any(char::is_control)
        }) {
            out["accountId"] = json!(id);
            out["accountIdentity"] = json!({"issuer":PORTAL,"subject":id});
        }
        for key in ["email", "name"] {
            if let Some(v) = c[key]
                .as_str()
                .filter(|v| !v.is_empty() && v.len() <= 512 && !v.chars().any(char::is_control))
            {
                out[key] = json!(v);
            }
        }
    }
    if !usable(&out, started, 0) {
        out["expires"] = json!(1);
    }
    Ok(out)
}

pub(crate) fn transient(status: u16, headers: &BTreeMap<String, String>) -> bool {
    status == 408
        || status == 429
        || status >= 500
        || status == 403
            && headers
                .keys()
                .any(|k| k.eq_ignore_ascii_case("x-vercel-mitigated"))
}
pub(crate) fn refused(
    status: u16,
    headers: &BTreeMap<String, String>,
    body: &Value,
    refreshing: bool,
) -> PluginError {
    if transient(status, headers) {
        return error("temporarily_unavailable", false, status);
    }
    let raw = text(body, "error");
    let dead = ["invalid_grant", "invalid_token", "refresh_token_reused"].contains(&raw);
    let known = dead
        || [
            "access_denied",
            "expired_token",
            "authorization_pending",
            "slow_down",
            "invalid_client",
            "invalid_scope",
        ]
        .contains(&raw);
    error(
        if known { raw } else { "oauth_request_failed" },
        refreshing && dead,
        status,
    )
}
pub(crate) fn form_request(
    path: &str,
    fields: &[(&str, &str)],
    headers: BTreeMap<String, String>,
) -> HttpRequest {
    HttpRequest {
        method: HttpMethod::Post,
        url: format!("{PORTAL}{path}"),
        headers: BTreeMap::from([
            ("accept".into(), "application/json".into()),
            (
                "content-type".into(),
                "application/x-www-form-urlencoded".into(),
            ),
        ])
        .into_iter()
        .chain(headers)
        .collect(),
        body: Some(
            url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(fields.iter().copied())
                .finish()
                .into_bytes(),
        ),
    }
}
type Shared = futures::future::Shared<BoxFuture<'static, PluginResult<Value>>>;
#[derive(Default)]
struct SessionState {
    fields: Value,
    pending: Option<Shared>,
    error: Option<PluginError>,
    retry_at: i64,
}
#[derive(Clone, Default)]
pub(crate) struct Rotations {
    sessions: Arc<Mutex<BTreeMap<String, Arc<Mutex<SessionState>>>>>,
}
fn fingerprint(auth: &Value) -> PluginResult<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(metadata(auth)?.token.as_bytes())
    ))
}
impl Rotations {
    async fn session(&self, auth: &Value) -> PluginResult<Arc<Mutex<SessionState>>> {
        let key = fingerprint(auth)?;
        let mut sessions = self.sessions.lock().await;
        if let Some(s) = sessions.get(&key) {
            return Ok(s.clone());
        }
        if sessions.len() >= 4096 {
            return Err(error("too_many_active_sessions", false, 0));
        }
        let state = Arc::new(Mutex::new(SessionState {
            fields: auth.clone(),
            ..SessionState::default()
        }));
        sessions.insert(key, state.clone());
        Ok(state)
    }
    pub(crate) async fn refresh(
        &self,
        auth: &Value,
        client: Arc<dyn HttpClient>,
        force: bool,
    ) -> PluginResult<Value> {
        if auth["type"] != "oauth" {
            return Err(error("not_signed_in", true, 0));
        }
        let session = self.session(auth).await?;
        let mut state = session.lock().await;
        let force = force && text(auth, "access") == text(&state.fields, "access");
        if !force && usable(&state.fields, now(), LEAD) {
            return Ok(state.fields.clone());
        }
        if let Some(pending) = &state.pending {
            let pending = pending.clone();
            drop(state);
            return Self::await_refresh(pending, session, force).await;
        }
        if state.retry_at > now() {
            if !force
                && state
                    .error
                    .as_ref()
                    .is_none_or(|e| e.code != "authentication_required")
                && usable(&state.fields, now(), 0)
            {
                return Ok(state.fields.clone());
            }
            return Err(state
                .error
                .clone()
                .unwrap_or_else(|| error("temporarily_unavailable", false, 0)));
        }
        nora_provider_plugin_sdk::before_http_request().await?;
        let old = state.fields.clone();
        let book = self.clone();
        let target = session.clone();
        let request = form_request(
            "/api/oauth/token",
            &[("grant_type", "refresh_token"), ("client_id", CLIENT)],
            BTreeMap::from([("x-nous-refresh-token".into(), metadata(&old)?.token)]),
        );
        let task = tokio::spawn(async move {
            let started = now();
            let response = async {
                let (status, headers, body) = http::json(
                    http::unmetered(client.as_ref(), request, Duration::from_secs(10)).await?,
                )
                .await?;
                if !(200..300).contains(&status) || !body.is_object() || has_error(&body) {
                    return Err(refused(status, &headers, &body, true));
                }
                token(&body, Some(&old), started)
            }
            .await;
            if let Ok(next) = &response {
                book.sessions
                    .lock()
                    .await
                    .insert(fingerprint(next)?, target.clone());
            }
            let mut state = target.lock().await;
            state.pending = None;
            match &response {
                Ok(next) => {
                    state.fields = next.clone();
                    state.error = None;
                    state.retry_at = 0;
                    if !usable(next, now(), LEAD) {
                        state.error = Some(error("unusable_inference_jwt", false, 0));
                        state.retry_at = now() + 30_000;
                    }
                }
                Err(e) => {
                    state.error = Some(e.clone());
                    state.retry_at = if e.code == "authentication_required" {
                        i64::MAX
                    } else {
                        now() + 30_000
                    };
                }
            }
            response
        });
        let pending = async move {
            task.await
                .map_err(|_| error("token_endpoint_unavailable", false, 0))?
        }
        .boxed()
        .shared();
        state.pending = Some(pending.clone());
        drop(state);
        Self::await_refresh(pending, session, force).await
    }
    async fn await_refresh(
        pending: Shared,
        session: Arc<Mutex<SessionState>>,
        force: bool,
    ) -> PluginResult<Value> {
        match pending.await {
            Ok(next) => Ok(next),
            Err(e) if !force && e.code != "authentication_required" => {
                let state = session.lock().await;
                if usable(&state.fields, now(), 0) {
                    Ok(state.fields.clone())
                } else {
                    Err(e)
                }
            }
            Err(e) => Err(e),
        }
    }
}

pub(crate) struct DeviceFlow {
    pub code: String,
    pub interval: Duration,
    pub deadline: tokio::time::Instant,
    pub canceled: tokio::sync::watch::Sender<bool>,
    pub result: Mutex<Option<Shared>>,
}
impl Drop for DeviceFlow {
    fn drop(&mut self) {
        let _ = self.canceled.send(true);
    }
}
pub(crate) async fn device(client: &dyn HttpClient) -> PluginResult<(Value, DeviceFlow)> {
    let started = tokio::time::Instant::now();
    let (status, headers, body) = http::json(
        http::send(
            client,
            form_request(
                "/api/oauth/device/code",
                &[("client_id", CLIENT), ("scope", SCOPE)],
                BTreeMap::new(),
            ),
            Duration::from_secs(10),
        )
        .await?,
    )
    .await?;
    if !(200..300).contains(&status) || !body.is_object() || has_error(&body) {
        return Err(refused(status, &headers, &body, false));
    }
    let expires = body["expires_in"]
        .as_f64()
        .filter(|n| n.is_finite() && *n > 0.);
    let interval = body
        .get("interval")
        .map(|v| v.as_f64().filter(|n| n.is_finite() && *n > 0.))
        .unwrap_or(Some(5.));
    if text(&body, "device_code").is_empty()
        || text(&body, "user_code").is_empty()
        || expires.is_none()
        || interval.is_none()
    {
        return Err(error("invalid_device_code_response", false, 0));
    }
    let plain = verification_url(text(&body, "verification_uri"))?;
    let url = if !text(&body, "verification_uri_complete").is_empty() {
        verification_url(text(&body, "verification_uri_complete"))?
    } else {
        plain
    };
    let (canceled, _) = tokio::sync::watch::channel(false);
    Ok((
        json!({"url":url,"method":"auto","userCode":text(&body,"user_code"),"instructions":format!("Sign in to Nous Portal in your browser and confirm code {}. Finish any CAPTCHA in the browser. This finishes automatically.",text(&body,"user_code"))}),
        DeviceFlow {
            code: text(&body, "device_code").into(),
            interval: Duration::from_secs_f64(interval.unwrap().min(1800.)),
            deadline: started + Duration::from_secs_f64(expires.unwrap().min(1800.)),
            canceled,
            result: Mutex::new(None),
        },
    ))
}
pub(crate) async fn callback(
    flow: &DeviceFlow,
    client: Arc<dyn HttpClient>,
) -> PluginResult<Value> {
    let mut result = flow.result.lock().await;
    if let Some(pending) = &*result {
        let pending = pending.clone();
        drop(result);
        return pending.await;
    }
    let code = flow.code.clone();
    let interval = flow.interval;
    let deadline = flow.deadline;
    let mut canceled = flow.canceled.subscribe();
    let future=async move {
        let work=poll(client,&code,interval,deadline);
        tokio::select!{result=work=>result,_=canceled.changed()=>Err(error("authorization_canceled",false,0))}
    }.boxed().shared();
    *result = Some(future.clone());
    drop(result);
    future.await
}
async fn poll(
    client: Arc<dyn HttpClient>,
    code: &str,
    mut interval: Duration,
    deadline: tokio::time::Instant,
) -> PluginResult<Value> {
    let mut wait = interval;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(error("expired_token", false, 0));
        }
        tokio::time::sleep(wait.min(remaining)).await;
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(error("expired_token", false, 0));
        }
        let at = now();
        let response = async {
            http::json(
                http::send(
                    client.as_ref(),
                    form_request(
                        "/api/oauth/token",
                        &[
                            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                            ("client_id", CLIENT),
                            ("device_code", code),
                        ],
                        BTreeMap::new(),
                    ),
                    remaining.min(Duration::from_secs(10)),
                )
                .await?,
            )
            .await
        }
        .await;
        let (status, headers, body) = match response {
            Ok(r) => r,
            Err(_) => {
                wait = wait.saturating_mul(2).max(interval);
                continue;
            }
        };
        if (200..300).contains(&status) && body.is_object() && !has_error(&body) {
            let token = token(&body, None, at)?;
            assert_current(&token, now(), LEAD)?;
            return Ok(token);
        }
        if transient(status, &headers) {
            let after = headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("retry-after"))
                .map(|(_, v)| {
                    v.parse::<f64>()
                        .ok()
                        .filter(|n| n.is_finite())
                        .map(|n| (n * 1000.).max(0.) as u64)
                        .or_else(|| {
                            chrono::DateTime::parse_from_rfc2822(v)
                                .ok()
                                .map(|d| d.timestamp_millis().saturating_sub(now()).max(0) as u64)
                        })
                })
                .flatten()
                .unwrap_or(0);
            wait = wait
                .saturating_mul(2)
                .max(interval)
                .max(Duration::from_millis(after));
            continue;
        }
        match text(&body, "error") {
            "authorization_pending" => wait = interval,
            "slow_down" => {
                interval = interval.saturating_add(Duration::from_secs(5));
                wait = interval;
            }
            _ => return Err(refused(status, &headers, &body, false)),
        }
    }
}

#[cfg(test)]
#[path = "../test/auth.test.rs"]
pub(crate) mod tests;
