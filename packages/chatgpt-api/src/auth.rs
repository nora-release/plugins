use crate::http;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use futures::{future::BoxFuture, FutureExt};
use nora_ai::{HttpClient, HttpMethod, HttpRequest};
use nora_provider_auth::{
    browser_sign_in::BrowserSignIn, OAuthCallbackServer, OAuthCallbackServerConfig,
};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};
use tokio::sync::Mutex;

pub(crate) const ISSUER: &str = "https://auth.openai.com";
pub(crate) const BASE: &str = "https://api.openai.com/v1";
pub(crate) const DIRECT: &str = "chatgpt.tokens.use.direct";
pub(crate) const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
pub(crate) const LEAD: i64 = 180_000;
pub(crate) fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
pub(crate) fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
fn failed(message: &str) -> PluginError {
    PluginError::transport(format!("ChatGPT sign-in: {message}"))
}
#[derive(Clone)]
pub(crate) struct Endpoints {
    pub issuer: String,
    pub api: String,
    pub callback: String,
}
impl Default for Endpoints {
    fn default() -> Self {
        Self {
            issuer: ISSUER.into(),
            api: BASE.into(),
            callback: "127.0.0.1:1455".into(),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Metadata {
    pub client_id: String,
    pub host_id: String,
    pub subject: String,
    pub email: String,
    pub id_token: String,
    pub scopes: Vec<String>,
}
pub(crate) fn metadata(value: &Value) -> PluginResult<Metadata> {
    let meta: Metadata = serde_json::from_value(value["extras"].clone())
        .map_err(|_| failed("The saved account is unreadable. Sign in again."))?;
    if meta.client_id.is_empty()
        || meta.host_id.is_empty()
        || text(value, "access").is_empty()
        || text(value, "refresh").is_empty()
    {
        return Err(failed("The saved account is incomplete. Sign in again."));
    }
    Ok(meta)
}
pub(crate) fn claims(token: &str) -> Value {
    let parts = token.split('.').collect::<Vec<_>>();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        return Value::Null;
    }
    URL_SAFE_NO_PAD
        .decode(parts[1].trim_end_matches('='))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(Value::is_object)
        .unwrap_or(Value::Null)
}
pub(crate) fn host_id(directory: &Path) -> PluginResult<String> {
    use std::io::Write;
    let path = directory.join("chatgpt-api-host");
    let read = || {
        std::fs::read_to_string(&path)
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| {
                v.strip_prefix("urn:uuid:")
                    .is_some_and(|v| uuid::Uuid::parse_str(v).is_ok())
            })
    };
    if let Some(id) = read() {
        return Ok(id);
    }
    std::fs::create_dir_all(directory)
        .map_err(|_| failed("The private account directory could not be created."))?;
    let id = format!("urn:uuid:{}", uuid::Uuid::new_v4());
    let temporary = directory.join(format!(".host-{}", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temporary)?;
        file.write_all(format!("{id}\n").as_bytes())?;
        file.sync_all()?;
        match std::fs::hard_link(&temporary, &path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(error),
        }
    })();
    let _ = std::fs::remove_file(&temporary);
    result.map_err(|_| failed("The machine's ChatGPT client identity could not be saved."))?;
    read().ok_or_else(|| failed("The machine's ChatGPT client identity is invalid."))
}
pub(crate) fn authorize_url(
    endpoints: &Endpoints,
    redirect: &str,
    state: &str,
    nonce: &str,
    challenge: &str,
    host: &str,
) -> String {
    let mut url = url::Url::parse(&format!("{}/api/accounts/authorize", endpoints.issuer)).unwrap();
    url.query_pairs_mut().extend_pairs([
        ("client_id", "dynamic_agent_client"),
        ("agent_name_hint", "Nora"),
        ("ext_agent_host_id", host),
        ("response_type", "code"),
        ("redirect_uri", redirect),
        ("resource", BASE),
        ("scope", SCOPES),
        ("state", state),
        ("nonce", nonce),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
    ]);
    url.to_string()
}
fn random() -> PluginResult<String> {
    let mut b = [0u8; 32];
    getrandom::getrandom(&mut b)
        .map_err(|_| failed("Secure sign-in randomness is unavailable."))?;
    Ok(URL_SAFE_NO_PAD.encode(b))
}
pub(crate) fn token_request(endpoints: &Endpoints, fields: &[(&str, &str)]) -> HttpRequest {
    HttpRequest {
        method: HttpMethod::Post,
        url: format!("{}/api/accounts/oauth/token", endpoints.issuer),
        headers: BTreeMap::from([
            (
                "content-type".into(),
                "application/x-www-form-urlencoded".into(),
            ),
            ("accept".into(), "application/json".into()),
        ]),
        body: Some(
            url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(fields.iter().copied())
                .finish()
                .into_bytes(),
        ),
    }
}
pub(crate) fn refusal(status: u16, body: &Value) -> PluginError {
    let code = body["code"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| body["error"].as_str())
        .or_else(|| body.pointer("/error/code").and_then(Value::as_str))
        .unwrap_or("");
    if [
        "invalid_grant",
        "invalid_refresh_token",
        "token_expired",
        "refresh_token_expired",
        "refresh_token_invalidated",
        "refresh_token_reused",
    ]
    .contains(&code)
    {
        return PluginError::authentication("The ChatGPT sign-in has expired. Sign in again.");
    }
    if code == "invalid_client" {
        return failed("OpenAI no longer knows this registered client. Sign in again.");
    }
    failed(&format!(
        "OpenAI could not complete the token request (HTTP {status}). Credentials were kept."
    ))
}
pub(crate) fn from_token(
    body: &Value,
    old: Option<&Value>,
    meta: Metadata,
    at: i64,
) -> PluginResult<Value> {
    let access = text(body, "access_token");
    let refresh = text(body, "refresh_token");
    if access.is_empty() || refresh.is_empty() {
        return Err(failed("OpenAI sent no token."));
    }
    let life = body["expires_in"]
        .as_f64()
        .filter(|v| v.is_finite() && *v > 0.)
        .ok_or_else(|| failed("OpenAI's token has no lifetime."))?;
    let scopes = text(body, "scope")
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if !scopes.iter().any(|v| v == DIRECT) {
        return Err(failed("OpenAI did not grant chatgpt.tokens.use.direct; this ChatGPT account may not be eligible."));
    }
    let mut meta = meta;
    meta.scopes = scopes;
    if !text(body, "id_token").is_empty() {
        meta.id_token = text(body, "id_token").into();
    }
    let mut saved = old
        .cloned()
        .unwrap_or_else(|| json!({"type":"oauth","authMethod":"oauth","provider":"chatgpt-api"}));
    saved["access"] = json!(access);
    saved["refresh"] = json!(refresh);
    saved["expires"] = json!((at as f64 + life * 1000.).min(i64::MAX as f64) as i64);
    let mut extras = saved["extras"].as_object().cloned().unwrap_or_default();
    extras.extend(
        serde_json::to_value(&meta)
            .unwrap()
            .as_object()
            .unwrap()
            .clone(),
    );
    saved["extras"] = Value::Object(extras);
    if !meta.subject.is_empty() {
        saved["accountId"] = json!(meta.subject);
        saved["accountIdentity"] =
            json!({"issuer":ISSUER,"subject":meta.subject,"scope":"chatgpt-api"});
    } else if !meta.email.is_empty() {
        saved["accountIdentity"] =
            json!({"issuer":ISSUER,"subject":format!("email:{}",meta.email),"scope":"chatgpt-api"});
    }
    if !meta.email.is_empty() {
        saved["email"] = json!(meta.email);
    }
    for token in [&meta.id_token, access] {
        if let Some(plan) = claims(token)
            .pointer("/https:~1~1api.openai.com~1auth/chatgpt_plan_type")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            saved["plan"] = json!(plan);
            break;
        }
    }
    Ok(saved)
}
pub(crate) fn check_id(
    id: &Value,
    issuer: &str,
    client: &str,
    nonce: &str,
    at: i64,
) -> PluginResult<()> {
    if !id.is_object() {
        return Err(failed("OpenAI sent an unreadable ID token."));
    }
    if text(id, "iss").trim_end_matches('/') != issuer.trim_end_matches('/') {
        return Err(failed("The ID token is not from OpenAI."));
    }
    let audience = id["aud"].as_str().is_some_and(|v| v == client)
        || id["aud"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == client));
    if !audience {
        return Err(failed("The ID token is for another app."));
    }
    if text(id, "nonce") != nonce {
        return Err(failed("The ID token is not this sign-in's."));
    }
    if id["exp"]
        .as_f64()
        .is_some_and(|exp| exp * 1000. < (at - 60_000) as f64)
    {
        return Err(failed("The ID token has expired."));
    }
    Ok(())
}
pub(crate) struct Flow {
    pub deadline: tokio::time::Instant,
    result: Mutex<BrowserSignIn<Value>>,
    task: tokio::task::AbortHandle,
}
impl Flow {
    pub(crate) fn cancel(&self) {
        self.task.abort();
    }
}
impl Drop for Flow {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub(crate) async fn authorize(
    endpoints: &Endpoints,
    directory: &Path,
    client: Arc<dyn HttpClient>,
) -> PluginResult<(Value, Flow)> {
    let host = host_id(directory)?;
    let verifier = random()?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random()?;
    let nonce = random()?;
    let address: std::net::SocketAddr = endpoints
        .callback
        .parse()
        .map_err(|_| failed("The callback address is invalid."))?;
    let mut config = OAuthCallbackServerConfig::with_path(address.port(), "/auth/callback");
    config.host = address.ip().to_string();
    let mut server = OAuthCallbackServer::with_config(config);
    server.expect_state(&state);
    server.success_html = "<p>Sign-in received. Return to Nora to finish.</p>".into();
    if server.start().await.is_err() {
        server.config.port = 0;
        server
            .start()
            .await
            .map_err(|_| failed("The local callback port could not be opened."))?;
    }
    let redirect = server.redirect_uri();
    let input = server
        .callback_input()
        .expect("A ChatGPT sign-in has a state");
    let url = authorize_url(endpoints, &redirect, &state, &nonce, &challenge, &host);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(900);
    let endpoints = endpoints.clone();
    let task = tokio::spawn(async move {
        tokio::time::timeout_at(deadline, async {
            let pairs = server
                .wait_for_callback()
                .await
                .map_err(|_| failed("The browser did not authorize this sign-in."))?;
            exchange_callback(
                client.as_ref(),
                &endpoints,
                &pairs,
                &redirect,
                &nonce,
                &verifier,
                &host,
            )
            .await
        })
        .await
        .unwrap_or_else(|_| Err(failed("The sign-in timed out.")))
    });
    let abort = task.abort_handle();
    Ok((
        json!({"url":url,"method":"auto","pasteCallback":true,"callbackMode":"poll","instructions":"Complete the ChatGPT sign-in in your browser."}),
        Flow {
            deadline,
            result: Mutex::new(BrowserSignIn::new(task, Some(input))),
            task: abort,
        },
    ))
}
async fn exchange_callback(
    client: &dyn HttpClient,
    endpoints: &Endpoints,
    pairs: &BTreeMap<String, String>,
    redirect: &str,
    nonce: &str,
    verifier: &str,
    host: &str,
) -> PluginResult<Value> {
    let code = pairs
        .get("code")
        .ok_or_else(|| failed("OpenAI returned no authorization code."))?;
    let client_id = pairs
        .get("client_id")
        .filter(|value| {
            !value.trim().is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| {
            failed("OpenAI did not finish registering Nora (no client id came back). Start again.")
        })?;
    let (status, _, body) = http::json(
        http::send(
            client,
            token_request(
                endpoints,
                &[
                    ("grant_type", "authorization_code"),
                    ("client_id", client_id),
                    ("code", code),
                    ("code_verifier", verifier),
                    ("redirect_uri", redirect),
                    ("resource", BASE),
                ],
            ),
            Duration::from_secs(30),
        )
        .await?,
        1024 * 1024,
    )
    .await?;
    if status != 200 {
        return Err(refusal(status, &body));
    }
    let id = claims(text(&body, "id_token"));
    check_id(&id, &endpoints.issuer, client_id, nonce, now())?;
    let meta = Metadata {
        client_id: client_id.clone(),
        host_id: host.into(),
        subject: text(&id, "sub").into(),
        email: text(&id, "email").into(),
        id_token: text(&body, "id_token").into(),
        scopes: Vec::new(),
    };
    if meta.subject.is_empty() && meta.email.is_empty() {
        return Err(failed(
            "OpenAI did not identify the account that signed in.",
        ));
    }
    from_token(&body, None, meta, now())
}
pub(crate) async fn callback(flow: &Flow) -> PluginResult<Value> {
    flow.result.lock().await.finish().await
}
pub(crate) async fn poll(
    flow: &Flow,
    session: &str,
    params: &Value,
) -> PluginResult<Option<Value>> {
    flow.result.lock().await.pending_reply(session, params)
}

type Shared = futures::future::Shared<BoxFuture<'static, PluginResult<Value>>>;
#[derive(Default)]
struct RefreshState {
    fields: Value,
    pending: Option<Shared>,
}
#[derive(Clone, Default)]
pub(crate) struct Rotations {
    sessions: Arc<Mutex<BTreeMap<String, Arc<Mutex<RefreshState>>>>>,
}
fn fingerprint(auth: &Value) -> String {
    format!("{:x}", Sha256::digest(text(auth, "refresh").as_bytes()))
}
fn fresh(auth: &Value) -> bool {
    auth["expires"]
        .as_i64()
        .is_some_and(|exp| exp > now() + LEAD)
}
impl Rotations {
    pub(crate) async fn refresh(
        &self,
        auth: &Value,
        client: Arc<dyn HttpClient>,
        endpoints: Endpoints,
        force: bool,
    ) -> PluginResult<Value> {
        metadata(auth)?;
        let session = {
            let mut all = self.sessions.lock().await;
            let key = fingerprint(auth);
            if let Some(s) = all.get(&key) {
                s.clone()
            } else {
                if all.len() > 4096 {
                    return Err(failed("Too many account refreshes are active."));
                }
                let s = Arc::new(Mutex::new(RefreshState {
                    fields: auth.clone(),
                    pending: None,
                }));
                all.insert(key, s.clone());
                s
            }
        };
        let mut state = session.lock().await;
        let force = force && text(auth, "access") == text(&state.fields, "access");
        if !force && fresh(&state.fields) {
            return Ok(state.fields.clone());
        }
        if let Some(pending) = &state.pending {
            let pending = pending.clone();
            drop(state);
            return pending.await;
        }
        nora_provider_plugin_sdk::before_http_request().await?;
        let old = state.fields.clone();
        let meta = metadata(&old)?;
        let book = self.clone();
        let target = session.clone();
        let request = token_request(
            &endpoints,
            &[
                ("grant_type", "refresh_token"),
                ("client_id", &meta.client_id),
                ("refresh_token", text(&old, "refresh")),
                ("resource", BASE),
            ],
        );
        let task = tokio::spawn(async move {
            let result = async {
                let (status, _, body) = http::json(
                    http::unmetered(client.as_ref(), request, Duration::from_secs(30)).await?,
                    1024 * 1024,
                )
                .await?;
                if status != 200 {
                    return Err(refusal(status, &body));
                }
                from_token(&body, Some(&old), meta, now())
            }
            .await;
            let result = match result {
                Err(e)
                    if !force
                        && e.code != "authentication_required"
                        && old["expires"].as_i64().is_some_and(|exp| exp > now()) =>
                {
                    Ok(old)
                }
                other => other,
            };
            if let Ok(next) = &result {
                book.sessions
                    .lock()
                    .await
                    .insert(fingerprint(next), target.clone());
            }
            let mut state = target.lock().await;
            state.pending = None;
            if let Ok(next) = &result {
                state.fields = next.clone();
            }
            result
        });
        let pending = async move {
            task.await
                .map_err(|_| failed("The token refresh stopped."))?
        }
        .boxed()
        .shared();
        state.pending = Some(pending.clone());
        drop(state);
        pending.await
    }
}
pub(crate) async fn revoke(
    client: &dyn HttpClient,
    endpoints: &Endpoints,
    saved: &Value,
) -> PluginResult<()> {
    let meta = metadata(saved)?;
    let (status, _, discovery) = http::json(
        http::send(
            client,
            HttpRequest::new(
                HttpMethod::Get,
                format!("{}/.well-known/openid-configuration", endpoints.issuer),
            ),
            Duration::from_secs(15),
        )
        .await?,
        1024 * 1024,
    )
    .await?;
    if status != 200 {
        return Err(failed("OpenAI's revocation service is unavailable."));
    }
    let endpoint = url::Url::parse(text(&discovery, "revocation_endpoint"))
        .map_err(|_| failed("OpenAI named no revocation endpoint."))?;
    let issuer =
        url::Url::parse(&endpoints.issuer).map_err(|_| failed("The OpenAI issuer is invalid."))?;
    if endpoint.origin() != issuer.origin()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(failed("OpenAI returned an invalid revocation endpoint."));
    }
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs([
            ("token", text(saved, "refresh")),
            ("token_type_hint", "refresh_token"),
            ("client_id", &meta.client_id),
        ])
        .finish()
        .into_bytes();
    let response = http::send(
        client,
        HttpRequest::new(HttpMethod::Post, endpoint.to_string())
            .with_header("content-type", "application/x-www-form-urlencoded")
            .with_body(body),
        Duration::from_secs(15),
    )
    .await?;
    if response.status != 200 {
        return Err(failed("OpenAI could not revoke this sign-in."));
    }
    Ok(())
}

#[cfg(test)]
#[path = "../test/auth.test.rs"]
pub(crate) mod tests;
