//! JoyCode IDE gateway protocol adapted from magpie-community/plugins (MIT).
mod login;
mod models;
mod response;
mod usage;
use async_trait::async_trait;
use futures::StreamExt;
use nora_ai::{HttpClient, HttpMethod, HttpRequest, HttpStreamingResponse, ReqwestHttpClient};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, NetworkProxy, PluginError,
    PluginResult, ProviderPlugin, API_VERSION,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::sync::{Mutex, OnceCell};
const BASE: &str = "https://api-ai.jd.com";
const LOGIN_TYPE: &str = "PIN_JD_CLOUD";
// Public JoyCode IDE protocol constant, not an account credential.
const SIGN_KEY: &str = "0691a3f0b37b4a85aeb63ad0fc7db3ed";
#[derive(Clone)]
struct Config {
    manifest: Value,
    proxy: Option<NetworkProxy>,
}
#[derive(Default)]
struct JoyCode {
    config: RwLock<Option<Config>>,
    pending: Mutex<BTreeMap<String, login::Flow>>,
    identities: Mutex<BTreeMap<String, Arc<OnceCell<Value>>>>,
    client: Option<Arc<dyn HttpClient>>,
}
impl JoyCode {
    fn config(&self) -> PluginResult<Config> {
        self.config
            .read()
            .map_err(|_| invalid("JoyCode settings are unavailable."))?
            .clone()
            .ok_or_else(|| invalid("Initialize JoyCode first."))
    }
    fn client(&self, proxy: Option<&NetworkProxy>) -> PluginResult<Arc<dyn HttpClient>> {
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        let config = self.config()?;
        Ok(Arc::new(
            ReqwestHttpClient::with_client(nora_provider_plugin_sdk::http_client(
                proxy.or(config.proxy.as_ref()),
            )?)
            .with_observer_fn(|| async {
                nora_provider_plugin_sdk::before_http_request()
                    .await
                    .map_err(|_| {
                        nora_ai::HttpClientError::Transport("JoyCode request was cancelled.".into())
                    })
            }),
        ))
    }
    fn params_client(&self, params: &Value) -> PluginResult<Arc<dyn HttpClient>> {
        let proxy: Option<NetworkProxy> =
            serde_json::from_value(params.get("networkProxy").cloned().unwrap_or(Value::Null))
                .map_err(|_| invalid("JoyCode network settings are invalid."))?;
        self.client(proxy.as_ref())
    }
    async fn identity(
        &self,
        client: &dyn HttpClient,
        credentials: &Value,
        account: &str,
    ) -> PluginResult<Value> {
        let ticket = ticket(credentials)?;
        let key = format!(
            "{:x}",
            Sha256::digest(
                format!(
                    "{account}\0{ticket}\0{}\0{}",
                    meta(credentials, "tenant"),
                    meta(credentials, "loginType")
                )
                .as_bytes()
            )
        );
        let cell = {
            let mut cache = self.identities.lock().await;
            if cache.len() >= 128 && !cache.contains_key(&key) {
                if let Some(first) = cache.keys().next().cloned() {
                    cache.remove(&first);
                }
            }
            cache.entry(key).or_default().clone()
        };
        cell.get_or_try_init(|| async {
            let user = query(client, credentials, "joycode_userInfo", json!({})).await?;
            user_identity(&user, credentials)
        })
        .await
        .cloned()
    }
}
#[async_trait]
impl ProviderPlugin for JoyCode {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        if params.api_version != API_VERSION
            || params.plugin_id != "joycode"
            || text(&params.manifest, "id") != "joycode"
        {
            return Err(invalid("The JoyCode executable and manifest do not match."));
        }
        let proxy = serde_json::from_value(
            params
                .manifest
                .pointer("/contributes/provider/config/networkProxy")
                .cloned()
                .unwrap_or(Value::Null),
        )
        .map_err(|_| invalid("JoyCode network settings are invalid."))?;
        *self
            .config
            .write()
            .map_err(|_| invalid("JoyCode settings are unavailable."))? = Some(Config {
            manifest: params.manifest,
            proxy,
        });
        Ok(
            json!({"apiVersion":nora_provider_plugin_sdk::API_VERSION,"capabilities":{"persistent":true}}),
        )
    }
    async fn auth_methods(&self, _: CallContext) -> PluginResult<Value> {
        Ok(self.config()?.manifest["contributes"]["provider"]["authMethods"].clone())
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        login::authorize(self, params).await
    }
    async fn callback(&self, params: Value) -> PluginResult<Value> {
        login::callback(self, params).await
    }
    async fn accounts(&self, context: CallContext) -> PluginResult<Value> {
        let Some(credentials) = context.credentials else {
            return Ok(json!({"accounts":[]}));
        };
        let client = self.client(context.network_proxy.as_ref())?;
        let user = query(client.as_ref(), &credentials, "joycode_userInfo", json!({})).await?;
        let next = credentials_for(
            ticket(&credentials)?,
            meta(&credentials, "loginType"),
            meta(&credentials, "tenant"),
            &user,
        )?;
        Ok(
            json!({"accounts":[{"id":context.account,"accountId":next["accountId"],"name":next["name"],"configured":true}],"credentials":next}),
        )
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        models::load(self, context).await
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let credentials = context.credentials.as_ref().ok_or_else(signed_out)?;
        let client = self.client(context.network_proxy.as_ref())?;
        usage::parse(&query(client.as_ref(), credentials, "get_newpoint_ide", json!({})).await?)
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        if request.method != "POST" || request.url != format!("{BASE}/v1/chat/completions") {
            return Err(invalid("JoyCode only accepts its official chat endpoint."));
        }
        let credentials = request.credentials.as_ref().ok_or_else(signed_out)?;
        ticket(credentials)?;
        let client = self.client(request.network_proxy.as_ref())?;
        let bytes = request
            .decode_body()?
            .ok_or_else(|| invalid("JoyCode needs a chat request body."))?;
        if bytes.len() > 32 * 1024 * 1024 {
            return Err(invalid("The JoyCode request exceeds 32 MiB."));
        }
        let mut body: Value = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("The JoyCode chat body is invalid JSON."))?;
        if !body.is_object() || text(&body, "model").is_empty() || !body["messages"].is_array() {
            return Err(invalid("JoyCode needs a model and messages."));
        }
        if self
            .config()?
            .manifest
            .pointer("/contributes/provider/config/flatten")
            == Some(&json!(true))
        {
            body = models::flatten(&body);
        }
        let identity = self
            .identity(client.as_ref(), credentials, &request.account)
            .await?;
        for key in ["userId", "tenant", "orgFullName"] {
            body[key] = identity[key].clone();
        }
        body["language"] = json!("UNKNOWN");
        if let Some(limit) = request.constraints.output_token_limit {
            body["max_tokens"] = json!(limit);
            body.as_object_mut()
                .unwrap()
                .remove("max_completion_tokens");
        }
        let mut headers = request
            .headers
            .into_iter()
            .filter(|(key, _)| {
                !matches!(
                    key.to_ascii_lowercase().as_str(),
                    "authorization"
                        | "proxy-authorization"
                        | "cookie"
                        | "host"
                        | "content-length"
                        | "content-encoding"
                )
            })
            .collect::<BTreeMap<_, _>>();
        headers.extend(headers_for(credentials, Some(text(&identity, "tenant")))?);
        headers.insert("content-type".into(), "application/json".into());
        let response = send(
            client.as_ref(),
            HttpRequest {
                method: HttpMethod::Post,
                url: signed_url("chat_completions", now(), SIGN_KEY),
                headers,
                body: Some(body.to_string().into_bytes()),
            },
            120,
        )
        .await?;
        response::normalize(response, &[ticket(credentials)?.to_owned()]).await
    }
}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
fn meta<'a>(credentials: &'a Value, key: &str) -> &'a str {
    credentials
        .get("metadata")
        .and_then(|value| value.get(key))
        .or_else(|| credentials.get(key))
        .and_then(Value::as_str)
        .unwrap_or("")
}
fn valid(text: &str, max: usize) -> bool {
    !text.is_empty() && text.len() <= max && !text.chars().any(char::is_control)
}
fn ticket(credentials: &Value) -> PluginResult<&str> {
    let key = text(credentials, "key");
    if !matches!(text(credentials, "type"), "api" | "apiKey") || !valid(key, 16384) {
        return Err(signed_out());
    }
    Ok(key)
}
fn invalid(message: impl Into<String>) -> PluginError {
    PluginError::invalid(message)
}
fn transport(message: impl Into<String>) -> PluginError {
    PluginError::transport(message)
}
fn signed_out() -> PluginError {
    PluginError::authentication("Sign in to JoyCode again.")
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
fn signed_url(function: &str, at: u64, key: &str) -> String {
    use hmac::{Hmac, Mac};
    let material = format!("joycode_ide&{function}&{at}");
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("HMAC accepts any key length");
    mac.update(material.as_bytes());
    let sign = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let mut url = url::Url::parse(&format!("{BASE}/api")).unwrap();
    url.query_pairs_mut()
        .append_pair("appid", "joycode_ide")
        .append_pair("functionId", function)
        .append_pair("t", &at.to_string())
        .append_pair("sign", &sign);
    url.into()
}
fn encode_component(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(byte) {
                (*byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}
fn headers_for(
    credentials: &Value,
    tenant: Option<&str>,
) -> PluginResult<BTreeMap<String, String>> {
    let mut headers = BTreeMap::from([("ptkey".into(), ticket(credentials)?.into())]);
    let kind = meta(credentials, "loginType");
    let kind = if kind.is_empty() { LOGIN_TYPE } else { kind };
    if !valid(kind, 256) {
        return Err(signed_out());
    }
    headers.insert("logintype".into(), kind.into());
    let tenant = tenant.unwrap_or_else(|| meta(credentials, "tenant")).trim();
    if !tenant.is_empty() {
        if !valid(tenant, 1024) {
            return Err(signed_out());
        }
        headers.insert("tenant".into(), encode_component(tenant));
    }
    Ok(headers)
}
async fn send(
    client: &dyn HttpClient,
    request: HttpRequest,
    seconds: u64,
) -> PluginResult<HttpStreamingResponse> {
    tokio::time::timeout(Duration::from_secs(seconds), client.stream(request))
        .await
        .map_err(|_| transport("JoyCode request timed out."))?
        .map_err(|_| transport("JoyCode could not be reached."))
}
async fn query(
    client: &dyn HttpClient,
    credentials: &Value,
    function: &str,
    body: Value,
) -> PluginResult<Value> {
    query_checked(client, credentials, function, body, false).await
}
async fn query_checked(
    client: &dyn HttpClient,
    credentials: &Value,
    function: &str,
    body: Value,
    strict: bool,
) -> PluginResult<Value> {
    let mut headers = headers_for(credentials, None)?;
    headers.insert(
        "content-type".into(),
        "application/json; charset=UTF-8".into(),
    );
    let response = send(
        client,
        HttpRequest {
            method: HttpMethod::Post,
            url: signed_url(function, now(), SIGN_KEY),
            headers,
            body: Some(body.to_string().into_bytes()),
        },
        15,
    )
    .await?;
    let status = response.status;
    let bytes = tokio::time::timeout(
        Duration::from_secs(30),
        nora_ai::collect_http_body_text_limited(response.body, 1024 * 1024),
    )
    .await
    .map_err(|_| transport("JoyCode data query timed out."))?
    .map_err(|_| transport("JoyCode returned incomplete data."))?;
    let value: Value = serde_json::from_str(&bytes).unwrap_or(Value::Null);
    if status == 401 || code_number(value.get("code")) == Some(401.0) {
        return Err(signed_out());
    }
    if !(200..300).contains(&status)
        || !value.is_object()
        || strict && value.get("code").and_then(Value::as_i64) != Some(0)
        || value
            .get("code")
            .is_some_and(|code| code_number(Some(code)) != Some(0.0))
    {
        let message = value
            .pointer("/error/message")
            .or_else(|| value.get("message"))
            .or_else(|| value.get("msg"))
            .and_then(Value::as_str)
            .map(|message| message.chars().take(500).collect::<String>())
            .unwrap_or_default();
        let message = message.replace(ticket(credentials)?, "[redacted]");
        return Err(transport(format!(
            "JoyCode data query failed (HTTP {status}). {message}"
        )));
    }
    Ok(value.get("data").cloned().unwrap_or(value))
}
fn code_number(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(|value| match value {
            Value::Null => Some(0.0),
            Value::Bool(value) => Some(if *value { 1.0 } else { 0.0 }),
            Value::String(value) if value.trim().is_empty() => Some(0.0),
            _ => value
                .as_f64()
                .or_else(|| value.as_str().and_then(|value| value.trim().parse().ok())),
        })
        .filter(|value| value.is_finite())
}
fn user_identity(user: &Value, credentials: &Value) -> PluginResult<Value> {
    let id = text(user, "userId");
    if !valid(id, 512) || id.trim() != id {
        return Err(transport(
            "JoyCode did not return a valid account identity.",
        ));
    }
    let tenant = if text(user, "tenant").is_empty() {
        meta(credentials, "tenant")
    } else {
        text(user, "tenant")
    }
    .trim();
    if tenant.len() > 512 || tenant.chars().any(char::is_control) {
        return Err(transport("JoyCode returned an invalid account scope."));
    }
    Ok(json!({"userId":id,"tenant":tenant,"orgFullName":text(user,"orgFullName")}))
}
fn credentials_for(key: &str, kind: &str, tenant: &str, user: &Value) -> PluginResult<Value> {
    let mut credentials = json!({"type":"apiKey","provider":"joycode","credentialProvider":"joycode","key":key,"metadata":{"loginType":if kind.is_empty(){LOGIN_TYPE}else{kind},"tenant":tenant}});
    ticket(&credentials)?;
    let name = text(user, "userName").to_owned();
    let user = user_identity(user, &credentials)?;
    let id = text(&user, "userId");
    let mut identity = json!({"issuer":BASE,"subject":id});
    if !text(&user, "tenant").is_empty() {
        identity["scope"] = user["tenant"].clone();
    }
    credentials["accountId"] = json!(id);
    credentials["accountIdentity"] = identity;
    credentials["name"] = json!(if valid(&name, 512) { name.as_str() } else { id });
    credentials["metadata"]["tenant"] = user["tenant"].clone();
    Ok(credentials)
}
#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = nora_provider_plugin_sdk::serve(JoyCode::default()).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
#[cfg(test)]
#[path = "../test/protocol.test.rs"]
mod tests;
