//! ChatGPT subscriptions for nora-gateway: an account signed in to with
//! ChatGPT, as Codex CLI does, whose requests go to ChatGPT's Codex backend
//! and count against the plan's allowance.

mod auth;
mod codex;

use futures::StreamExt;
use nora_plugin_runtime_sdk::protocol::RpcError;
use nora_plugin_runtime_sdk::provider::{
    Fetch, FetchHead, Model, Provider, Question, Renewed, SignIn, SignInFinish, Step, Usage,
};
use provider_support::{
    collect_body_text_limited, random_hex, HttpClient, HttpMethod, HttpRequest, PluginError,
    PluginResult, ReqwestHttpClient,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// The most of a request body read: images and long contexts fit.
const MAXIMUM_REQUEST_BYTES: usize = 64 * 1024 * 1024;
/// Where each model's instructions are kept, in the plugin's own folder.
const PROMPTS_FILE: &str = "codex-instructions.json";

pub struct ChatGpt {
    client: Option<Arc<dyn HttpClient>>,
    pending: Mutex<BTreeMap<String, auth::Pending>>,
    /// The instructions the backend wants with each model, as its list gave them.
    prompts: Mutex<BTreeMap<String, String>>,
    prompts_file: Option<PathBuf>,
    /// A refresh token is spent once: what its refresh gave, for a minute,
    /// to whoever asks with it again.
    renewed: Mutex<BTreeMap<String, (Value, Instant)>>,
}

impl Default for ChatGpt {
    fn default() -> Self {
        // the router runs the plugin in its own folder
        let prompts_file = std::env::current_dir()
            .ok()
            .map(|dir| dir.join(PROMPTS_FILE));
        let prompts = prompts_file
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            client: None,
            pending: Mutex::new(BTreeMap::new()),
            prompts: Mutex::new(prompts),
            prompts_file,
            renewed: Mutex::new(BTreeMap::new()),
        }
    }
}

impl ChatGpt {
    fn client(&self, proxy: Option<&str>) -> PluginResult<Arc<dyn HttpClient>> {
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        Ok(Arc::new(ReqwestHttpClient::new(proxy)?))
    }

    fn signed_in(&self, question: &Question) -> PluginResult<(Arc<dyn HttpClient>, Value)> {
        let credentials = question.credentials.clone().ok_or_else(auth::signed_out)?;
        if auth::text(&credentials, "access").is_empty() {
            return Err(auth::signed_out());
        }
        Ok((self.client(question.proxy.as_deref())?, credentials))
    }

    async fn renew(&self, client: &dyn HttpClient, saved: &Value) -> PluginResult<Value> {
        let key = auth::text(saved, "refresh").to_owned();
        let mut renewed = self.renewed.lock().await;
        renewed.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(60));
        if let Some((next, _)) = renewed.get(&key) {
            return Ok(next.clone());
        }
        let next = auth::refresh(client, saved).await?;
        renewed.insert(key, (next.clone(), Instant::now()));
        Ok(next)
    }

    async fn list(&self, client: &dyn HttpClient, credentials: &Value) -> PluginResult<Vec<Model>> {
        let request = codex::account_request(
            HttpRequest::new(
                HttpMethod::Get,
                format!("{}/models?client_version={}", codex::BASE, codex::VERSION),
            ),
            credentials,
        );
        let listed = codex::models(&codex::json(client, request, Duration::from_secs(30)).await?)?;
        let mut prompts = self.prompts.lock().await;
        let mut changed = false;
        for entry in &listed {
            if let Some(text) = &entry.instructions {
                if prompts.get(&entry.model.id) != Some(text) {
                    prompts.insert(entry.model.id.clone(), text.clone());
                    changed = true;
                }
            }
        }
        if changed {
            self.save_prompts(&prompts);
        }
        Ok(listed.into_iter().map(|entry| entry.model).collect())
    }

    fn save_prompts(&self, prompts: &BTreeMap<String, String>) {
        let Some(path) = &self.prompts_file else {
            return;
        };
        let Ok(bytes) = serde_json::to_vec(prompts) else {
            return;
        };
        let staged = path.with_extension(format!("{}.tmp", random_hex(4)));
        if std::fs::write(&staged, bytes).is_ok() {
            let _ = std::fs::rename(&staged, path);
        }
        let _ = std::fs::remove_file(staged);
    }

    async fn instructions(
        &self,
        client: &dyn HttpClient,
        credentials: &Value,
        model: &str,
    ) -> Option<String> {
        if let Some(text) = self.prompts.lock().await.get(model) {
            return Some(text.clone());
        }
        // a model asked before the list was read: read it once
        self.list(client, credentials).await.ok()?;
        self.prompts.lock().await.get(model).cloned()
    }

    async fn post(
        &self,
        client: &dyn HttpClient,
        credentials: &Value,
        body: &[u8],
        session: Option<&str>,
    ) -> PluginResult<provider_support::HttpStreamingResponse> {
        let mut request = codex::account_request(
            HttpRequest::new(HttpMethod::Post, format!("{}/responses", codex::BASE)),
            credentials,
        )
        .with_header("openai-beta", "responses=experimental")
        .with_header("version", codex::VERSION)
        .with_header("content-type", "application/json")
        .with_header("accept", "text/event-stream")
        .with_body(body.to_vec());
        if let Some(session) = session {
            request = request.with_header("session_id", session);
        }
        codex::send(client, request, Duration::from_secs(120)).await
    }
}

impl Provider for ChatGpt {
    async fn sign_in(&self, request: SignIn) -> Result<Step, RpcError> {
        if request.way != "browser" {
            return Err(PluginError::unsupported(format!("Signing in by {}", request.way)).into());
        }
        let client = self.client(request.question.proxy.as_deref())?;
        // one port, one sign-in: a new one takes it over, the one before
        // given up first so its port is free
        let mut sessions = self.pending.lock().await;
        sessions.clear();
        let (url, pending) = auth::start(client).await?;
        let session = random_hex(16);
        sessions.insert(session.clone(), pending);
        Ok(Step::Open {
            url,
            code: None,
            session,
        })
    }

    async fn sign_in_finish(&self, request: SignInFinish) -> Result<Step, RpcError> {
        let mut sessions = self.pending.lock().await;
        let pending = sessions
            .get_mut(&request.session)
            .ok_or_else(auth::signed_out)?;
        let Some(result) = pending.poll().await else {
            return Ok(Step::Waiting);
        };
        sessions.remove(&request.session);
        Ok(Step::Account(auth::account(result?)))
    }

    async fn refresh(&self, question: Question) -> Result<Renewed, RpcError> {
        let (client, saved) = self.signed_in(&question)?;
        let next = self.renew(client.as_ref(), &saved).await?;
        Ok(Renewed {
            expires_at: auth::expires_at(&next),
            credentials: next,
        })
    }

    async fn models(&self, question: Question) -> Result<Vec<Model>, RpcError> {
        let (client, credentials) = self.signed_in(&question)?;
        Ok(self.list(client.as_ref(), &credentials).await?)
    }

    async fn fetch(&self, mut request: Fetch) -> Result<FetchHead, RpcError> {
        if request.method != "POST" || request.url != format!("{}/responses", codex::BASE) {
            return Err(PluginError::invalid(
                "A ChatGPT account takes Responses requests at the Codex backend.",
            )
            .into());
        }
        let (client, mut credentials) = self.signed_in(&request.question)?;
        let bytes = request.body.read_to_end(MAXIMUM_REQUEST_BYTES).await?;
        let mut body: Value = serde_json::from_slice(&bytes)
            .map_err(|_| PluginError::invalid("The request body is not JSON."))?;
        let object = body
            .as_object_mut()
            .ok_or_else(|| PluginError::invalid("The request body is not an object."))?;
        let streamed = object.get("stream") == Some(&json!(true));
        let model = object
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let instructions = self
            .instructions(client.as_ref(), &credentials, &model)
            .await;
        codex::body(object, instructions);
        let session = object
            .get("prompt_cache_key")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let bytes = serde_json::to_vec(&body)
            .map_err(|_| PluginError::invalid("The request could not be encoded."))?;

        let mut renewed = None;
        let mut response = self
            .post(client.as_ref(), &credentials, &bytes, session.as_deref())
            .await?;
        if response.status == 401 {
            // an access token ChatGPT turned away early: renewed once
            credentials = self.renew(client.as_ref(), &credentials).await?;
            renewed = Some(credentials.clone());
            response = self
                .post(client.as_ref(), &credentials, &bytes, session.as_deref())
                .await?;
            if response.status == 401 {
                return Err(auth::signed_out().into());
            }
        }
        let mut head = FetchHead {
            status: response.status,
            expires_at: renewed.as_ref().and_then(auth::expires_at),
            credentials: renewed,
            ..FetchHead::default()
        };
        let mut output = request.response;
        if !(200..300).contains(&response.status) || !streamed {
            let text = collect_body_text_limited(response.body, 64 * 1024 * 1024)
                .await
                .map_err(|_| PluginError::transport("ChatGPT's answer was cut short."))?;
            let answer = if !(200..300).contains(&response.status) {
                let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                json!({"error": {"message": codex::explain(response.status, &value)}})
            } else {
                codex::completed(&text).ok_or_else(|| {
                    PluginError::transport("ChatGPT's answer ended without a response.")
                })?
            };
            head.headers
                .insert("content-type".into(), "application/json".into());
            let bytes = serde_json::to_vec(&answer).unwrap_or_default();
            tokio::spawn(async move {
                if output.write(&bytes).await.is_ok() {
                    let _ = output.finish().await;
                }
            });
            return Ok(head);
        }
        head.headers
            .insert("content-type".into(), "text/event-stream".into());
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
        let request = codex::account_request(
            HttpRequest::new(HttpMethod::Get, codex::USAGE_URL),
            &credentials,
        );
        let mut usage =
            codex::usage(&codex::json(client.as_ref(), request, Duration::from_secs(30)).await?);
        // when each reset runs out is a second look: without it, the count
        if let Some(resets) = usage.resets.as_mut() {
            let request = codex::account_request(
                HttpRequest::new(HttpMethod::Get, codex::RESET_CREDITS_URL),
                &credentials,
            );
            if let Ok(value) = codex::json(client.as_ref(), request, Duration::from_secs(4)).await {
                let cards = codex::reset_cards(&value);
                if cards.len() == resets.count as usize {
                    resets.cards = cards;
                }
            }
        }
        Ok(usage)
    }
}

/// Serves ChatGPT on stdin and stdout.
pub async fn serve() -> std::io::Result<()> {
    nora_plugin_runtime_sdk::provider::serve_provider(ChatGpt::default()).await
}

#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
