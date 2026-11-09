//! GitHub Copilot subscriptions for nora-gateway: a GitHub account signed
//! in to with the device flow, as Copilot's editors do, whose requests go to
//! Copilot's API and count against the plan's allowance.

mod copilot;
mod github;

use copilot::Session;
use futures::StreamExt;
use nora_plugin_runtime_sdk::protocol::RpcError;
use nora_plugin_runtime_sdk::provider::{
    Fetch, FetchHead, Model, Provider, Question, SignIn, SignInFinish, Step, Usage,
};
use provider_support::{
    random_hex, HttpClient, HttpMethod, HttpRequest, PluginError, PluginResult, ReqwestHttpClient,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// The most of a request body read: images and long contexts fit.
const MAXIMUM_REQUEST_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
pub struct Copilot {
    client: Option<Arc<dyn HttpClient>>,
    pending: Mutex<BTreeMap<String, github::Pending>>,
    /// Each GitHub token's session, kept until shortly before it expires.
    sessions: Mutex<BTreeMap<String, Session>>,
}

impl Copilot {
    fn client(&self, proxy: Option<&str>) -> PluginResult<Arc<dyn HttpClient>> {
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        Ok(Arc::new(ReqwestHttpClient::new(proxy)?))
    }

    fn signed_in(&self, question: &Question) -> PluginResult<(Arc<dyn HttpClient>, String)> {
        let token = question
            .credentials
            .as_ref()
            .and_then(|credentials| credentials["github"].as_str())
            .filter(|token| !token.is_empty())
            .ok_or_else(github::signed_out)?
            .to_owned();
        Ok((self.client(question.proxy.as_deref())?, token))
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

    async fn post(
        &self,
        client: &dyn HttpClient,
        session: &Session,
        path: &str,
        body: &[u8],
        initiator: &str,
        vision: bool,
    ) -> PluginResult<provider_support::HttpStreamingResponse> {
        let mut request = copilot::api_headers(
            HttpRequest::new(HttpMethod::Post, format!("{}{path}", session.api)),
            session,
        )
        .with_header("content-type", "application/json")
        .with_header("openai-intent", "conversation-panel")
        .with_header("x-initiator", initiator)
        .with_body(body.to_vec());
        if vision {
            request = request.with_header("copilot-vision-request", "true");
        }
        tokio::time::timeout(Duration::from_secs(120), client.stream(request))
            .await
            .map_err(|_| PluginError::transport("Copilot took too long to answer."))?
            .map_err(|error| {
                PluginError::transport(format!("Copilot could not be reached: {error}"))
            })
    }
}

impl Provider for Copilot {
    async fn sign_in(&self, request: SignIn) -> Result<Step, RpcError> {
        if request.way != "browser" {
            return Err(PluginError::unsupported(format!("Signing in by {}", request.way)).into());
        }
        let client = self.client(request.question.proxy.as_deref())?;
        let started = github::start(client.as_ref()).await?;
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
        let token = match github::poll(client.as_ref(), pending).await {
            Ok(Some(token)) => token,
            Ok(None) => return Ok(Step::Waiting),
            Err(error) => {
                sessions.remove(&request.session);
                return Err(error.into());
            }
        };
        sessions.remove(&request.session);
        drop(sessions);
        let account = github::account(client.as_ref(), &token).await?;
        // an account without Copilot is told now, not at its first request
        self.session(client.as_ref(), &token).await?;
        Ok(Step::Account(account))
    }

    async fn models(&self, question: Question) -> Result<Vec<Model>, RpcError> {
        let (client, token) = self.signed_in(&question)?;
        let session = self.session(client.as_ref(), &token).await?;
        let request = copilot::api_headers(
            HttpRequest::new(HttpMethod::Get, format!("{}/models", session.api)),
            &session,
        );
        let (status, value) =
            github::json(client.as_ref(), request, Duration::from_secs(30)).await?;
        if status == 401 {
            self.forget(&token).await;
        }
        if !(200..300).contains(&status) {
            return Err(PluginError::transport(github::explain(
                "Copilot did not list its models",
                status,
                &value,
            ))
            .into());
        }
        Ok(copilot::models(&value))
    }

    async fn fetch(&self, mut request: Fetch) -> Result<FetchHead, RpcError> {
        let path = request
            .url
            .strip_prefix(copilot::API)
            .filter(|path| request.method == "POST" && *path == "/chat/completions")
            .ok_or_else(|| {
                PluginError::invalid("A Copilot account takes Chat Completions requests.")
            })?
            .to_owned();
        let (client, token) = self.signed_in(&request.question)?;
        let bytes = request.body.read_to_end(MAXIMUM_REQUEST_BYTES).await?;
        let body: Value = serde_json::from_slice(&bytes)
            .map_err(|_| PluginError::invalid("The request body is not JSON."))?;
        let (initiator, vision) = (copilot::initiator(&body), copilot::has_image(&body));

        let mut session = self.session(client.as_ref(), &token).await?;
        let mut response = self
            .post(client.as_ref(), &session, &path, &bytes, initiator, vision)
            .await?;
        if response.status == 401 {
            // a session Copilot turned away before its time: traded again once
            self.forget(&token).await;
            session = self.session(client.as_ref(), &token).await?;
            response = self
                .post(client.as_ref(), &session, &path, &bytes, initiator, vision)
                .await?;
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
        let (client, token) = self.signed_in(&question)?;
        let request = copilot::editor_headers(HttpRequest::new(HttpMethod::Get, copilot::USER_URL))
            .with_header("authorization", format!("token {token}"))
            .with_header("accept", "application/json");
        let (status, value) =
            github::json(client.as_ref(), request, Duration::from_secs(30)).await?;
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
