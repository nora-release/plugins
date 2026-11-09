//! Grok subscriptions (SuperGrok, X Premium+) for nora-gateway: an xAI
//! account signed in to with the device flow, whose requests go to the
//! backend Grok Build talks to and count against the plan's credits.

mod grok;
mod xai;

use futures::StreamExt;
use nora_plugin_runtime_sdk::protocol::RpcError;
use nora_plugin_runtime_sdk::provider::{
    Fetch, FetchHead, Model, Provider, Question, Renewed, SignIn, SignInFinish, Step, Usage,
};
use provider_support::{
    collect_body_text_limited, explain, random_hex, DevicePending, HttpClient, HttpMethod,
    HttpRequest, HttpStreamingResponse, PluginError, PluginResult, ReqwestHttpClient,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// The most of a request body read: images and long contexts fit.
const MAXIMUM_REQUEST_BYTES: usize = 64 * 1024 * 1024;
/// A token this close to its end is renewed before a request.
const RENEW_MARGIN: Duration = Duration::from_secs(5 * 60);
/// How long Grok Build's stable version is kept once read.
const VERSION_KEPT: Duration = Duration::from_secs(6 * 3600);

#[derive(Default)]
pub struct Grok {
    client: Option<Arc<dyn HttpClient>>,
    pending: Mutex<BTreeMap<String, DevicePending>>,
    /// A refresh token is spent once: what its renewal gave, for a minute,
    /// to whoever asks with it again.
    renewed: Mutex<BTreeMap<String, (Value, Instant)>>,
    /// Grok Build's stable version, and when it was read.
    version: Mutex<Option<(String, Instant)>>,
}

impl Grok {
    fn client(&self, proxy: Option<&str>) -> PluginResult<Arc<dyn HttpClient>> {
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        Ok(Arc::new(ReqwestHttpClient::new(proxy)?))
    }

    fn signed_in(&self, question: &Question) -> PluginResult<(Arc<dyn HttpClient>, Value)> {
        let credentials = question.credentials.clone().ok_or_else(xai::signed_out)?;
        if xai::text(&credentials, "access").is_empty() {
            return Err(xai::signed_out());
        }
        Ok((self.client(question.proxy.as_deref())?, credentials))
    }

    /// The Grok Build version requests say they come from: its stable
    /// channel's, read now and then, else the one known.
    async fn version(&self, client: &dyn HttpClient) -> String {
        let mut kept = self.version.lock().await;
        if let Some((version, _)) = kept.as_ref().filter(|(_, at)| at.elapsed() < VERSION_KEPT) {
            return version.clone();
        }
        let mut read = None;
        for url in grok::STABLE_URLS {
            let request = HttpRequest::new(HttpMethod::Get, url);
            let Ok(Ok(response)) =
                tokio::time::timeout(Duration::from_secs(5), client.stream(request)).await
            else {
                continue;
            };
            if response.status != 200 {
                continue;
            }
            if let Ok(text) = collect_body_text_limited(response.body, 1024).await {
                read = grok::version(&text);
            }
            if read.is_some() {
                break;
            }
        }
        let version = read.unwrap_or_else(|| grok::VERSION.to_owned());
        *kept = Some((version.clone(), Instant::now()));
        version
    }

    async fn renew(&self, client: &dyn HttpClient, saved: &Value) -> PluginResult<Value> {
        let key = xai::text(saved, "refresh").to_owned();
        let mut renewed = self.renewed.lock().await;
        renewed.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(60));
        if let Some((next, _)) = renewed.get(&key) {
            return Ok(next.clone());
        }
        let version = self.version(client).await;
        let next = xai::refresh(client, saved, &version).await?;
        renewed.insert(key, (next.clone(), Instant::now()));
        Ok(next)
    }

    /// GETs a backend address signed, and reads its JSON answer.
    async fn read(
        &self,
        client: &dyn HttpClient,
        credentials: &Value,
        url: &str,
    ) -> PluginResult<(u16, Value)> {
        let version = self.version(client).await;
        let request = grok::sign(
            HttpRequest::new(HttpMethod::Get, url),
            xai::text(credentials, "access"),
            &version,
        )
        .with_header("accept", "application/json");
        let response = send(client, request, Duration::from_secs(30)).await?;
        let status = response.status;
        let text = collect_body_text_limited(response.body, 8 * 1024 * 1024)
            .await
            .map_err(|_| PluginError::transport("Grok's answer was cut short."))?;
        if status == 401 {
            return Err(xai::signed_out());
        }
        Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
    }

    async fn post(
        &self,
        client: &dyn HttpClient,
        credentials: &Value,
        outgoing: &Outgoing,
    ) -> PluginResult<HttpStreamingResponse> {
        let version = self.version(client).await;
        let mut request = grok::sign(
            HttpRequest::new(HttpMethod::Post, format!("{}/responses", grok::BASE)),
            xai::text(credentials, "access"),
            &version,
        )
        .with_header("content-type", "application/json")
        .with_body(outgoing.body.clone());
        for (name, value) in &outgoing.headers {
            request = request.with_header(name.clone(), value.clone());
        }
        send(client, request, Duration::from_secs(120)).await
    }
}

/// A Responses request as it goes to Grok: its body and the headers it
/// adds to the signed ones.
struct Outgoing {
    body: Vec<u8>,
    headers: Vec<(String, String)>,
}

impl Outgoing {
    /// The request shaped for Grok: what it turns away left out, the model
    /// and window the id names, and Grok Build's headers.
    fn of(bytes: &[u8]) -> PluginResult<Self> {
        let mut body: Value = serde_json::from_slice(bytes)
            .map_err(|_| PluginError::invalid("The request body is not JSON."))?;
        let object = body
            .as_object_mut()
            .ok_or_else(|| PluginError::invalid("The request body is not an object."))?;
        grok::rewrite(object);
        let mut headers: Vec<(String, String)> = grok::LOOP_CHECKS
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        let asked = object
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let (model, window) = grok::model_of(&asked);
        if !model.is_empty() {
            headers.push(("x-grok-model-override".into(), model.to_owned()));
            object.insert("model".into(), Value::String(model.to_owned()));
        }
        if window > 0 {
            headers.push(("x-grok-context-window".into(), window.to_string()));
        }
        if let Some(key) = object
            .get("prompt_cache_key")
            .and_then(Value::as_str)
            .filter(|key| !key.is_empty())
        {
            headers.push(("x-grok-conv-id".into(), key.to_owned()));
        }
        let body = serde_json::to_vec(&body)
            .map_err(|_| PluginError::invalid("The request could not be encoded."))?;
        Ok(Self { body, headers })
    }
}

async fn send(
    client: &dyn HttpClient,
    request: HttpRequest,
    timeout: Duration,
) -> PluginResult<HttpStreamingResponse> {
    tokio::time::timeout(timeout, client.stream(request))
        .await
        .map_err(|_| PluginError::transport("Grok took too long to answer."))?
        .map_err(|error| PluginError::transport(format!("Grok could not be reached: {error}")))
}

impl Provider for Grok {
    async fn sign_in(&self, request: SignIn) -> Result<Step, RpcError> {
        if request.way != "browser" {
            return Err(PluginError::unsupported(format!("Signing in by {}", request.way)).into());
        }
        let client = self.client(request.question.proxy.as_deref())?;
        let version = self.version(client.as_ref()).await;
        let started = xai::device_flow(&version).start(client.as_ref()).await?;
        let session = random_hex(16);
        let mut pending = self.pending.lock().await;
        // a sign-in given up is never finished: only the newest is kept
        pending.clear();
        pending.insert(session.clone(), started.pending);
        Ok(Step::Open {
            url: started.url,
            code: Some(started.code),
            session,
        })
    }

    async fn sign_in_finish(&self, request: SignInFinish) -> Result<Step, RpcError> {
        let client = self.client(request.question.proxy.as_deref())?;
        let version = self.version(client.as_ref()).await;
        let mut sessions = self.pending.lock().await;
        let pending = sessions
            .get_mut(&request.session)
            .ok_or_else(xai::signed_out)?;
        let tokens = match xai::device_flow(&version)
            .poll(client.as_ref(), pending)
            .await
        {
            Ok(Some(tokens)) => tokens,
            Ok(None) => return Ok(Step::Waiting),
            Err(error) => {
                sessions.remove(&request.session);
                return Err(error.into());
            }
        };
        sessions.remove(&request.session);
        Ok(Step::Account(xai::account(xai::credentials(
            &Value::Null,
            &tokens,
        )?)))
    }

    async fn refresh(&self, question: Question) -> Result<Renewed, RpcError> {
        let (client, saved) = self.signed_in(&question)?;
        let next = self.renew(client.as_ref(), &saved).await?;
        Ok(Renewed {
            expires_at: xai::expires_at(&next),
            credentials: next,
        })
    }

    async fn models(&self, question: Question) -> Result<Vec<Model>, RpcError> {
        let (client, credentials) = self.signed_in(&question)?;
        let url = format!("{}/models", grok::BASE);
        let (status, value) = self.read(client.as_ref(), &credentials, &url).await?;
        if !(200..300).contains(&status) {
            return Err(PluginError::transport(explain(
                "Grok did not list its models",
                status,
                &value,
            ))
            .into());
        }
        let models = grok::models(&value);
        if models.is_empty() {
            return Err(PluginError::transport("Grok listed no models.").into());
        }
        Ok(models)
    }

    async fn fetch(&self, mut request: Fetch) -> Result<FetchHead, RpcError> {
        if request.method != "POST" || request.url != format!("{}/responses", grok::BASE) {
            return Err(PluginError::invalid("A Grok account takes Responses requests.").into());
        }
        let (client, mut credentials) = self.signed_in(&request.question)?;
        let client = client.as_ref();
        let bytes = request.body.read_to_end(MAXIMUM_REQUEST_BYTES).await?;
        let outgoing = Outgoing::of(&bytes)?;

        let mut renewed = None;
        if xai::ending(&credentials, RENEW_MARGIN) {
            credentials = self.renew(client, &credentials).await?;
            renewed = Some(credentials.clone());
        }
        let mut response = self.post(client, &credentials, &outgoing).await?;
        if response.status == 401 && renewed.is_none() {
            // a token Grok turned away before its time: renewed once
            credentials = self.renew(client, &credentials).await?;
            renewed = Some(credentials.clone());
            response = self.post(client, &credentials, &outgoing).await?;
        }
        if response.status == 401 {
            return Err(xai::signed_out().into());
        }
        let mut head = FetchHead {
            status: response.status,
            expires_at: renewed.as_ref().and_then(xai::expires_at),
            credentials: renewed,
            ..FetchHead::default()
        };
        for (name, value) in &response.headers {
            let name = name.to_ascii_lowercase();
            if name == "content-type"
                || name == "retry-after"
                || name.contains("ratelimit")
                || name.starts_with("x-request-id")
            {
                head.headers.insert(name, value.clone());
            }
        }
        let mut output = request.response;
        let mut stream = response.body;
        tokio::spawn(async move {
            while let Some(chunk) = stream.next().await {
                let written = match chunk {
                    Ok(bytes) => output.write(&bytes).await,
                    Err(error) => {
                        let _ = output
                            .abort(PluginError::transport(error.to_string()).into())
                            .await;
                        return;
                    }
                };
                if written.is_err() {
                    return;
                }
            }
            let _ = output.finish().await;
        });
        Ok(head)
    }

    async fn usage(&self, question: Question) -> Result<Usage, RpcError> {
        let (client, credentials) = self.signed_in(&question)?;
        let url = format!("{}/billing?format=credits", grok::BASE);
        let (status, value) = self.read(client.as_ref(), &credentials, &url).await?;
        if !(200..300).contains(&status) {
            return Err(PluginError::transport(explain(
                "Grok did not say what has been used",
                status,
                &value,
            ))
            .into());
        }
        Ok(grok::usage(&value))
    }
}

/// Serves Grok on stdin and stdout.
pub async fn serve() -> std::io::Result<()> {
    nora_plugin_runtime_sdk::provider::serve_provider(Grok::default()).await
}

#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
