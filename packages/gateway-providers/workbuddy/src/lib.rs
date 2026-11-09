//! WorkBuddy subscriptions (Tencent's CodeBuddy) for nora-gateway: the
//! China site (`workbuddy`) and the international one (`workbuddy-ai`).
//! Accounts sign in in the browser; every request goes through this plugin,
//! which shapes it as WorkBuddy's chat endpoint wants and renames its tools
//! back in the reply. The protocol modules come from the Nora Router
//! package, itself adapted from magpie-community/plugins (MIT).

mod auth;
mod checkin;
mod models;
mod request;
mod stream;
mod tools;
mod usage;

use futures::StreamExt;
use nora_plugin_runtime_sdk::protocol::RpcError;
use nora_plugin_runtime_sdk::provider::{
    Checkin, CheckinOutcome, Fetch, FetchHead, Model, Provider, Question, Renewed, SignIn,
    SignInFinish, Step, Usage, UsageWindow,
};
use provider_support::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// The most of a request body read: images and long contexts fit.
const MAXIMUM_REQUEST_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Site {
    China,
    International,
}

impl Site {
    pub fn id(self) -> &'static str {
        match self {
            Self::China => "workbuddy",
            Self::International => "workbuddy-ai",
        }
    }
    fn of(provider: &str) -> Result<Self, RpcError> {
        match provider {
            "workbuddy" => Ok(Self::China),
            "workbuddy-ai" => Ok(Self::International),
            _ => Err(RpcError::invalid_params(format!(
                "This plugin has no provider {provider}"
            ))),
        }
    }
    fn endpoint(self) -> &'static str {
        match self {
            Self::China => "https://copilot.tencent.com",
            Self::International => "https://www.workbuddy.ai",
        }
    }
    fn base(self) -> String {
        format!("{}/v2", self.endpoint())
    }
}

/// One site's state: sign-ins waiting on their users, refresh tokens just
/// spent, and what WorkBuddy taught about request shapes it refuses.
pub struct WorkBuddy {
    site: Site,
    pending: Mutex<BTreeMap<String, auth::Pending>>,
    refreshed: Mutex<BTreeMap<String, (Value, u64)>>,
    learned: Mutex<request::Learned>,
    salt: String,
    client: Option<Arc<dyn HttpClient>>,
}

impl WorkBuddy {
    pub fn new(site: Site) -> Self {
        Self {
            site,
            pending: Mutex::new(BTreeMap::new()),
            refreshed: Mutex::new(BTreeMap::new()),
            learned: Mutex::new(request::Learned::default()),
            salt: random_id(),
            client: None,
        }
    }

    fn client(&self, proxy: Option<&str>) -> PluginResult<Arc<dyn HttpClient>> {
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        Ok(Arc::new(ReqwestHttpClient::new(proxy)?))
    }

    async fn call(
        &self,
        client: &dyn HttpClient,
        method: HttpMethod,
        path: &str,
        headers: BTreeMap<String, String>,
        body: Option<Value>,
    ) -> PluginResult<Value> {
        let request = HttpRequest {
            method,
            url: format!("{}{path}", self.site.endpoint()),
            headers: headers
                .into_iter()
                .chain([
                    ("user-agent".into(), "WorkBuddy/5.5.6".into()),
                    ("content-type".into(), "application/json".into()),
                    ("accept".into(), "application/json".into()),
                ])
                .collect(),
            body: body.map(|body| body.to_string().into_bytes()),
        };
        envelope(send(client, request, Duration::from_secs(30)).await?).await
    }
}

/// Both sites, as one plugin answering for either provider.
pub struct WorkBuddyPlugin {
    china: WorkBuddy,
    international: WorkBuddy,
}

impl Default for WorkBuddyPlugin {
    fn default() -> Self {
        Self {
            china: WorkBuddy::new(Site::China),
            international: WorkBuddy::new(Site::International),
        }
    }
}

impl WorkBuddyPlugin {
    fn site(&self, question: &Question) -> Result<&WorkBuddy, RpcError> {
        Ok(match Site::of(&question.provider)? {
            Site::China => &self.china,
            Site::International => &self.international,
        })
    }

    fn signed_in(
        &self,
        question: &Question,
    ) -> Result<(&WorkBuddy, Arc<dyn HttpClient>, Value), RpcError> {
        let provider = self.site(question)?;
        let credentials = question.credentials.clone().ok_or_else(signed_out)?;
        Ok((
            provider,
            provider.client(question.proxy.as_deref())?,
            credentials,
        ))
    }
}

impl Provider for WorkBuddyPlugin {
    async fn sign_in(&self, request: SignIn) -> Result<Step, RpcError> {
        if request.way != "browser" {
            return Err(PluginError::unsupported(format!("Signing in by {}", request.way)).into());
        }
        let provider = self.site(&request.question)?;
        let client = provider.client(request.question.proxy.as_deref())?;
        Ok(auth::sign_in(provider, client.as_ref()).await?)
    }

    async fn sign_in_finish(&self, request: SignInFinish) -> Result<Step, RpcError> {
        let provider = self.site(&request.question)?;
        let client = provider.client(request.question.proxy.as_deref())?;
        Ok(auth::finish(provider, client.as_ref(), &request.session).await?)
    }

    async fn refresh(&self, question: Question) -> Result<Renewed, RpcError> {
        let (provider, client, credentials) = self.signed_in(&question)?;
        let credentials = auth::renew(provider, client.as_ref(), &credentials).await?;
        Ok(Renewed {
            expires_at: auth::expires_at(&credentials),
            credentials,
        })
    }

    async fn models(&self, question: Question) -> Result<Vec<Model>, RpcError> {
        let (provider, client, credentials) = self.signed_in(&question)?;
        let credentials = auth::current(&credentials)?;
        Ok(models::fetch(provider, client.as_ref(), &credentials).await?)
    }

    async fn fetch(&self, mut request: Fetch) -> Result<FetchHead, RpcError> {
        let provider = self.site(&request.question)?;
        let body = request.body.read_to_end(MAXIMUM_REQUEST_BYTES).await?;
        let response = request::fetch(
            provider,
            FetchRequest {
                credentials: request.question.credentials.clone(),
                proxy: request.question.proxy.clone(),
                url: request.url,
                method: request.method,
                metadata: BTreeMap::new(),
                body: (!body.is_empty()).then_some(body),
            },
        )
        .await?;
        let FetchResponse {
            status,
            headers,
            mut body,
        } = response;
        let mut output = request.response;
        tokio::spawn(async move {
            while let Some(chunk) = body.next().await {
                let written = match chunk {
                    Ok(bytes) => output.write(&bytes).await,
                    Err(error) => {
                        let _ = output.abort(error.into()).await;
                        return;
                    }
                };
                if written.is_err() {
                    return;
                }
            }
            let _ = output.finish().await;
        });
        Ok(FetchHead {
            status,
            headers,
            ..FetchHead::default()
        })
    }

    async fn usage(&self, question: Question) -> Result<Usage, RpcError> {
        let (provider, client, credentials) = self.signed_in(&question)?;
        let credentials = auth::current(&credentials)?;
        let value = usage::fetch(provider, client.as_ref(), &credentials).await?;
        Ok(usage_of(&value))
    }

    async fn checkin(&self, question: Question) -> Result<Checkin, RpcError> {
        let (provider, client, credentials) = self.signed_in(&question)?;
        let value = checkin::run(provider, client.as_ref(), &credentials, false).await?;
        Ok(checkin_of(&value))
    }
}

/// The usage module's answer as the router reads it.
fn usage_of(value: &Value) -> Usage {
    let windows = value
        .get("windows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|window| UsageWindow {
            name: text(window, "name").into(),
            used: window.get("used").and_then(number),
            resets_at: window
                .get("resetsAt")
                .and_then(Value::as_str)
                .map(str::to_owned),
            balance: match (
                window.get("limit").and_then(number),
                window.get("amount").and_then(number),
            ) {
                (Some(limit), Some(amount)) => Some((limit - amount).max(0.0)),
                _ => None,
            },
            unit: window
                .get("unit")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
        .collect();
    Usage {
        plan: value.get("plan").and_then(Value::as_str).map(str::to_owned),
        windows,
        resets: None,
    }
}

/// The check-in module's outcome as the router reads it.
fn checkin_of(value: &Value) -> Checkin {
    let outcome = match text(value, "outcome") {
        "claimed" => CheckinOutcome::Claimed,
        "done" => CheckinOutcome::Done,
        "captcha" => CheckinOutcome::Captcha,
        "failed" => CheckinOutcome::Failed,
        _ => CheckinOutcome::Unavailable,
    };
    Checkin {
        outcome,
        credit: value.get("credit").and_then(number),
        message: None,
    }
}

/// Serves both sites on stdin and stdout.
pub async fn serve() -> std::io::Result<()> {
    nora_plugin_runtime_sdk::provider::serve_provider(WorkBuddyPlugin::default()).await
}

fn invalid(message: &str) -> PluginError {
    PluginError::invalid(message)
}
fn signed_out() -> PluginError {
    PluginError::authentication("Sign in to this WorkBuddy account again.")
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
fn random_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).expect("The operating system random source is unavailable");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        .filter(|number| number.is_finite())
}
async fn send(
    client: &dyn HttpClient,
    request: HttpRequest,
    timeout: Duration,
) -> PluginResult<HttpStreamingResponse> {
    tokio::time::timeout(timeout, client.stream(request))
        .await
        .map_err(|_| PluginError::transport("The WorkBuddy request timed out."))?
        .map_err(|_| PluginError::transport("The WorkBuddy connection stopped."))
}
async fn bytes(
    response: HttpStreamingResponse,
    limit: usize,
) -> PluginResult<(u16, BTreeMap<String, String>, Vec<u8>)> {
    let status = response.status;
    let headers = response.headers;
    let body = tokio::time::timeout(
        Duration::from_secs(30),
        collect_body_text_limited(response.body, limit),
    )
    .await
    .map_err(|_| PluginError::transport("WorkBuddy did not finish its reply."))?
    .map_err(|_| PluginError::transport("WorkBuddy returned an incomplete or oversized reply."))?;
    Ok((status, headers, body.into_bytes()))
}
async fn envelope(response: HttpStreamingResponse) -> PluginResult<Value> {
    let (status, _, body) = bytes(response, 8 * 1024 * 1024).await?;
    if status == 401 {
        return Err(signed_out());
    }
    let value: Value = serde_json::from_slice(&body)
        .map_err(|_| PluginError::transport("WorkBuddy returned invalid JSON."))?;
    let code = number(value.get("code").unwrap_or(&Value::Null)).unwrap_or(0.0) as i64;
    if !(200..300).contains(&status) || code != 0 {
        return Err(PluginError::new(
            format!("workbuddy_{code}"),
            format!(
                "WorkBuddy returned HTTP {status}, code {code}: {}",
                text(&value, "msg")
            ),
        ));
    }
    value
        .get("data")
        .cloned()
        .ok_or_else(|| PluginError::transport("WorkBuddy returned no response data."))
}

#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
