//! GitHub Copilot subscriptions for nora-gateway: a GitHub account signed
//! in to with the device flow, as Copilot's editors do, whose requests go to
//! Copilot's API and count against the plan's allowance.

mod auto;
mod copilot;
mod github;
mod refused;

use auto::Pick;
use copilot::Session;
use futures::StreamExt;
use nora_plugin_runtime_sdk::protocol::RpcError;
use nora_plugin_runtime_sdk::provider::{
    Fetch, FetchHead, Model, Provider, Question, SignIn, SignInFinish, Step, Usage,
};
use provider_support::{
    collect_body_text_limited, random_hex, DevicePending, HttpClient, HttpMethod, HttpRequest,
    HttpStreamingResponse, PluginError, PluginResult, ReqwestHttpClient,
};
use refused::Refused;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// The most of a request body read: images and long contexts fit.
const MAXIMUM_REQUEST_BYTES: usize = 64 * 1024 * 1024;

pub struct Copilot {
    client: Option<Arc<dyn HttpClient>>,
    pending: Mutex<BTreeMap<String, DevicePending>>,
    /// Each GitHub token's session, kept until shortly before it expires.
    sessions: Mutex<BTreeMap<String, Session>>,
    /// Each GitHub token's Auto pick.
    picks: Mutex<BTreeMap<String, Pick>>,
    /// The models each account was refused picked by hand.
    refused: Mutex<Refused>,
}

impl Default for Copilot {
    fn default() -> Self {
        // the router runs the plugin in its own folder
        let path = std::env::current_dir()
            .ok()
            .map(|dir| dir.join(refused::FILE));
        Self {
            client: None,
            pending: Mutex::default(),
            sessions: Mutex::default(),
            picks: Mutex::default(),
            refused: Mutex::new(Refused::load(path)),
        }
    }
}

/// A signed-in account: its GitHub token and login.
struct Signed {
    client: Arc<dyn HttpClient>,
    token: String,
    login: String,
}

impl Copilot {
    fn client(&self, proxy: Option<&str>) -> PluginResult<Arc<dyn HttpClient>> {
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        Ok(Arc::new(ReqwestHttpClient::new(proxy)?))
    }

    fn signed_in(&self, question: &Question) -> PluginResult<Signed> {
        let credentials = question
            .credentials
            .as_ref()
            .ok_or_else(github::signed_out)?;
        let token = credentials["github"]
            .as_str()
            .filter(|token| !token.is_empty())
            .ok_or_else(github::signed_out)?;
        Ok(Signed {
            client: self.client(question.proxy.as_deref())?,
            token: token.to_owned(),
            login: credentials["login"].as_str().unwrap_or(token).to_owned(),
        })
    }

    /// The GitHub token's Copilot session: the one kept, or a new one.
    async fn session(&self, client: &dyn HttpClient, token: &str) -> PluginResult<Session> {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get(token).filter(|session| session.fresh()) {
            return Ok(session.clone());
        }
        let request =
            copilot::editor_headers(HttpRequest::new(HttpMethod::Get, copilot::TOKEN_URL))
                .with_header("authorization", format!("token {token}"))
                .with_header("accept", "application/json");
        let (status, value) = github::json(client, request, Duration::from_secs(30)).await?;
        if status == 401 {
            return Err(github::signed_out());
        }
        if status == 403 || status == 404 {
            return Err(PluginError::authentication(github::explain(
                "This GitHub account has no Copilot subscription",
                status,
                &value,
            )));
        }
        let session = copilot::session(&value)
            .filter(|_| (200..300).contains(&status))
            .ok_or_else(|| {
                PluginError::transport(github::explain(
                    "GitHub gave no Copilot session",
                    status,
                    &value,
                ))
            })?;
        sessions.insert(token.to_owned(), session.clone());
        Ok(session)
    }

    async fn forget(&self, token: &str) {
        self.sessions.lock().await.remove(token);
    }

    /// The account's Auto pick: the one kept, or one asked anew.
    async fn pick(
        &self,
        client: &dyn HttpClient,
        token: &str,
        session: &Session,
        fresh: bool,
    ) -> PluginResult<Pick> {
        let mut picks = self.picks.lock().await;
        if let Some(pick) = picks.get(token).filter(|pick| !fresh && pick.usable()) {
            return Ok(pick.clone());
        }
        let pick = auto::ask(client, session).await?;
        picks.insert(token.to_owned(), pick.clone());
        Ok(pick)
    }

    async fn post(
        &self,
        client: &dyn HttpClient,
        session: &Session,
        outgoing: &Outgoing,
    ) -> PluginResult<provider_support::HttpStreamingResponse> {
        let mut request = copilot::api_headers(
            HttpRequest::new(
                HttpMethod::Post,
                format!("{}{}", session.api, outgoing.path),
            ),
            session,
        )
        .with_header("content-type", "application/json")
        .with_header("openai-intent", "conversation-panel")
        .with_body(outgoing.body.clone());
        for (name, value) in &outgoing.headers {
            request = request.with_header(name.clone(), value.clone());
        }
        tokio::time::timeout(Duration::from_secs(120), client.stream(request))
            .await
            .map_err(|_| PluginError::transport("Copilot took too long to answer."))?
            .map_err(|error| {
                PluginError::transport(format!("Copilot could not be reached: {error}"))
            })
    }
}

/// A request as it goes to Copilot: its path there, its body and the
/// headers it adds to the session's.
struct Outgoing {
    path: &'static str,
    body: Vec<u8>,
    headers: Vec<(String, String)>,
}

impl Outgoing {
    /// The request as Auto's pick: the model it named, the session it carries.
    fn picked(&self, body: &Value, pick: &Pick) -> PluginResult<Self> {
        let mut body = body.clone();
        body["model"] = Value::String(pick.model.clone());
        let mut headers = self.headers.clone();
        headers.push(("copilot-session-token".into(), pick.token.clone()));
        Ok(Self {
            path: self.path,
            body: serde_json::to_vec(&body)
                .map_err(|_| PluginError::invalid("The request could not be encoded."))?,
            headers,
        })
    }
}

/// What Copilot answered a request, its error read when it has one.
async fn answer(response: HttpStreamingResponse) -> PluginResult<(HttpStreamingResponse, Value)> {
    if (200..300).contains(&response.status) {
        return Ok((response, Value::Null));
    }
    let status = response.status;
    let headers = response.headers;
    let text = collect_body_text_limited(response.body, 1024 * 1024)
        .await
        .map_err(|_| PluginError::transport("Copilot's answer was cut short."))?;
    let value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let bytes = text.into_bytes();
    let body: provider_support::HttpByteStream =
        Box::pin(futures::stream::once(async move { Ok(bytes) }));
    Ok((
        HttpStreamingResponse {
            status,
            headers,
            body,
        },
        value,
    ))
}

/// The caller's headers sent on to Copilot: Anthropic's version and betas.
const PASSED_HEADERS: &[&str] = &["anthropic-version", "anthropic-beta"];

impl Provider for Copilot {
    async fn sign_in(&self, request: SignIn) -> Result<Step, RpcError> {
        if request.way != "browser" {
            return Err(PluginError::unsupported(format!("Signing in by {}", request.way)).into());
        }
        let client = self.client(request.question.proxy.as_deref())?;
        let started = github::device_flow().start(client.as_ref()).await?;
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
        let mut sessions = self.pending.lock().await;
        let pending = sessions
            .get_mut(&request.session)
            .ok_or_else(github::signed_out)?;
        let tokens = match github::device_flow().poll(client.as_ref(), pending).await {
            Ok(Some(tokens)) => tokens,
            Ok(None) => return Ok(Step::Waiting),
            Err(error) => {
                sessions.remove(&request.session);
                return Err(error.into());
            }
        };
        sessions.remove(&request.session);
        drop(sessions);
        let token = tokens["access_token"].as_str().unwrap_or_default();
        let account = github::account(client.as_ref(), token).await?;
        // an account without Copilot is told now, not at its first request
        self.session(client.as_ref(), token).await?;
        Ok(Step::Account(account))
    }

    async fn models(&self, question: Question) -> Result<Vec<Model>, RpcError> {
        let signed = self.signed_in(&question)?;
        let client = signed.client.as_ref();
        let session = self.session(client, &signed.token).await?;
        let request = copilot::api_headers(
            HttpRequest::new(HttpMethod::Get, format!("{}/models", session.api)),
            &session,
        );
        let (status, value) = github::json(client, request, Duration::from_secs(30)).await?;
        if status == 401 {
            self.forget(&signed.token).await;
        }
        if !(200..300).contains(&status) {
            return Err(PluginError::transport(github::explain(
                "Copilot did not list its models",
                status,
                &value,
            ))
            .into());
        }
        let refused = self.refused.lock().await;
        let mut models: Vec<Model> = copilot::models(&value)
            .into_iter()
            .filter(|model| !refused.contains(&signed.login, &model.id))
            .collect();
        drop(refused);
        // Auto, which Copilot's clients offer every account beside the
        // models it lists, on the APIs its pick is served on
        if let Ok(pick) = self.pick(client, &signed.token, &session, false).await {
            models.insert(0, copilot::auto(&value, &pick.model));
        }
        Ok(models)
    }

    async fn fetch(&self, mut request: Fetch) -> Result<FetchHead, RpcError> {
        let (api, path) = request
            .url
            .strip_prefix(copilot::API)
            .filter(|_| request.method == "POST")
            .and_then(copilot::route)
            .ok_or_else(|| {
                PluginError::invalid(
                    "A Copilot account takes Chat Completions, Responses and Messages requests.",
                )
            })?;
        let signed = self.signed_in(&request.question)?;
        let (client, token) = (signed.client.as_ref(), signed.token.as_str());
        let bytes = request.body.read_to_end(MAXIMUM_REQUEST_BYTES).await?;
        let body: Value = serde_json::from_slice(&bytes)
            .map_err(|_| PluginError::invalid("The request body is not JSON."))?;
        let model = body["model"].as_str().unwrap_or_default().to_owned();
        let mut headers = vec![(
            "x-initiator".to_owned(),
            copilot::initiator(api, &body).to_owned(),
        )];
        if copilot::has_image(&body) {
            headers.push(("copilot-vision-request".into(), "true".into()));
        }
        for (name, value) in &request.headers {
            let name = name.to_ascii_lowercase();
            if PASSED_HEADERS.contains(&name.as_str()) {
                headers.push((name, value.clone()));
            }
        }
        let asked = Outgoing {
            path,
            body: bytes,
            headers,
        };

        let mut session = self.session(client, token).await?;
        let mut outgoing = if model == auto::AUTO {
            asked.picked(&body, &self.pick(client, token, &session, false).await?)?
        } else {
            asked
        };
        let mut response = self.post(client, &session, &outgoing).await?;
        if response.status == 401 {
            // a session Copilot turned away before its time: traded again once
            self.forget(token).await;
            session = self.session(client, token).await?;
            response = self.post(client, &session, &outgoing).await?;
        }
        let (mut response, mut error) = answer(response).await?;
        if model == auto::AUTO && auto::session_refused(response.status, &error) {
            // Auto's session let go or its pick changed: Copilot picks again
            let pick = self.pick(client, token, &session, true).await?;
            outgoing = outgoing.picked(&body, &pick)?;
            (response, error) = answer(self.post(client, &session, &outgoing).await?).await?;
        }
        if model != auto::AUTO && refused::refusal(response.status, &error) {
            self.refused.lock().await.add(&signed.login, &model);
            let bytes = serde_json::to_vec(&refused::explained(&model)).unwrap_or_default();
            response.body = Box::pin(futures::stream::once(async move { Ok(bytes) }));
            response
                .headers
                .insert("content-type".into(), "application/json".into());
        }
        let mut head = FetchHead {
            status: response.status,
            ..FetchHead::default()
        };
        for (name, value) in &response.headers {
            let name = name.to_ascii_lowercase();
            if name == "content-type" || name == "retry-after" || name.starts_with("x-ratelimit") {
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
        let signed = self.signed_in(&question)?;
        let request = copilot::editor_headers(HttpRequest::new(HttpMethod::Get, copilot::USER_URL))
            .with_header("authorization", format!("token {}", signed.token))
            .with_header("accept", "application/json");
        let (status, value) =
            github::json(signed.client.as_ref(), request, Duration::from_secs(30)).await?;
        if status == 401 {
            return Err(github::signed_out().into());
        }
        if !(200..300).contains(&status) {
            return Err(PluginError::transport(github::explain(
                "GitHub did not say what Copilot has used",
                status,
                &value,
            ))
            .into());
        }
        Ok(copilot::usage(&value))
    }
}

/// Serves GitHub Copilot on stdin and stdout.
pub async fn serve() -> std::io::Result<()> {
    nora_plugin_runtime_sdk::provider::serve_provider(Copilot::default()).await
}

#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
