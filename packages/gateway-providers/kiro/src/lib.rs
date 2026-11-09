//! Kiro subscriptions for nora-gateway. Accounts sign in in the browser
//! (Google, GitHub, AWS Builder ID or IAM Identity Center, the browser
//! coming back to a port on this machine) or with an API key. Every request
//! goes through this plugin, which turns Anthropic Messages into Kiro's
//! CodeWhisperer calls and its event stream back. The protocol modules come
//! from the Nora Router package.

mod auth;
mod network;
mod request;
mod session;
mod stream;
mod usage;

use futures::StreamExt;
use nora_plugin_runtime_sdk::protocol::RpcError;
use nora_plugin_runtime_sdk::provider::{
    Account, Fetch, FetchHead, Model, Provider, Question, Renewed, SignIn, SignInFinish, Step,
    Usage,
};
use provider_support::*;
use serde_json::{json, Value};
use session::Credential;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};
use tokio::sync::Mutex;

/// The most of a request body read: images and long contexts fit.
const MAXIMUM_REQUEST_BYTES: usize = 64 * 1024 * 1024;

pub struct Kiro {
    client: Option<Arc<dyn HttpClient>>,
    /// Browser sign-ins waiting on their users, by session.
    pending: Mutex<BTreeMap<String, auth::Pending>>,
    /// Each account's CodeWhisperer profile, looked up once.
    profiles: Mutex<BTreeMap<String, String>>,
    /// Each model's context window, as the model list said.
    windows: Mutex<BTreeMap<String, u64>>,
    /// Stands for this installation in Kiro's user agent.
    installation: String,
}

impl Default for Kiro {
    fn default() -> Self {
        Self {
            client: None,
            pending: Mutex::new(BTreeMap::new()),
            profiles: Mutex::new(BTreeMap::new()),
            windows: Mutex::new(BTreeMap::new()),
            installation: random_hex(16),
        }
    }
}

impl Kiro {
    fn client(&self, proxy: Option<&str>) -> PluginResult<Arc<dyn HttpClient>> {
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        Ok(Arc::new(ReqwestHttpClient::new(proxy)?))
    }

    /// The account's credentials ready for a call: renewed when the router
    /// has not yet, with its profile. The renewed ones, if any, go back to
    /// the router.
    async fn live(
        &self,
        question: &Question,
        client: &dyn HttpClient,
    ) -> PluginResult<(Credential, Option<Credential>)> {
        let saved = question.credentials.as_ref().ok_or_else(session::expired)?;
        let mut credential = Credential::from_saved(saved)?;
        let mut renewed = None;
        if !credential.fresh() {
            credential = renew(client, &credential, saved).await?;
            renewed = Some(credential.clone());
        }
        if credential.profile.is_empty() {
            credential.profile = self.profile(client, &credential).await?;
        }
        credential.runtime_region()?;
        Ok((credential, renewed))
    }

    async fn profile(
        &self,
        client: &dyn HttpClient,
        credential: &Credential,
    ) -> PluginResult<String> {
        let key = if credential.identity.is_empty() {
            network::key_identity(&credential.access)
        } else {
            credential.identity.clone()
        };
        if let Some(profile) = self.profiles.lock().await.get(&key) {
            return Ok(profile.clone());
        }
        let profile = network::profile(client, credential).await?;
        let mut profiles = self.profiles.lock().await;
        if profiles.len() >= 64 {
            profiles.clear();
        }
        profiles.insert(key, profile.clone());
        Ok(profile)
    }

    async fn send(
        &self,
        client: &dyn HttpClient,
        credential: &Credential,
        body: &Value,
    ) -> PluginResult<HttpStreamingResponse> {
        let region = credential.runtime_region()?;
        let ua = format!(
            "aws-sdk-rust/1.0.0 ua/2.1 os/other lang/rust api/codewhispererstreaming#1.28.3 m/E app/AmazonQ-For-CLI md/appVersion-1.28.3-{}",
            random_hex(16)
        );
        let outgoing = network::authorize(
            network::post(
                format!("https://runtime.{region}.kiro.dev/generateAssistantResponse"),
                body,
            )?,
            credential,
        )
        .with_header("accept", "application/vnd.amazon.eventstream")
        .with_header("x-amzn-codewhisperer-optout", "true")
        .with_header("amz-sdk-invocation-id", request::uuid())
        .with_header("amz-sdk-request", "attempt=1; max=1")
        .with_header("x-amzn-kiro-agent-mode", "vibe")
        .with_header("x-amz-user-agent", &ua)
        .with_header("user-agent", &ua);
        tokio::time::timeout(Duration::from_secs(60), client.stream(outgoing))
            .await
            .map_err(|_| PluginError::transport("The Kiro request timed out."))?
            .map_err(|_| PluginError::transport("The Kiro service could not be reached."))
    }

    /// Only Anthropic Messages at Kiro's own address go through.
    fn check_address(&self, method: &str, address: &str) -> PluginResult<()> {
        let url = url::Url::parse(address)
            .map_err(|_| PluginError::invalid("The Kiro request address is invalid."))?;
        if method != "POST"
            || url.origin() != url::Url::parse(session::BASE).unwrap().origin()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || !url
                .query_pairs()
                .all(|(key, value)| key == "beta" && value == "true")
            || !["/v1/messages", "/messages"].contains(&url.path())
        {
            return Err(PluginError::invalid(
                "Kiro accepts Anthropic Messages requests at its official endpoint.",
            ));
        }
        Ok(())
    }

    async fn sign_in_with_key(
        &self,
        client: &dyn HttpClient,
        inputs: &BTreeMap<String, String>,
    ) -> PluginResult<Step> {
        let mut credential = Credential::api_key(&json!(inputs))?;
        credential.identity = network::key_identity(&credential.access);
        let mut verified = credential.clone();
        if network::identify(client, &mut verified).await.is_ok() {
            credential = verified;
        } else {
            // a key works without its account's details; its profile only
            // picks the region later calls go to
            credential.profile = verified.profile;
        }
        Ok(Step::Account(account(&credential)))
    }
}

/// Renews an account's access, its identity read again when the renewal
/// moved it to another profile. A renewal that lands on another account
/// than the one saved is refused: the account must sign in again.
async fn renew(
    client: &dyn HttpClient,
    credential: &Credential,
    saved: &Value,
) -> PluginResult<Credential> {
    if credential.method == "apikey" {
        return Ok(credential.clone());
    }
    let mut next = network::refresh(client, credential).await?;
    if next.profile != credential.profile
        || next.realm != credential.realm
        || next.identity.is_empty()
    {
        network::identify(client, &mut next).await?;
    }
    if !next.matches_saved_identity(saved) {
        return Err(session::expired());
    }
    Ok(next)
}

fn expires_at(credential: &Credential) -> Option<String> {
    (credential.method != "apikey" && credential.expires > 0).then(|| {
        nora_plugin_runtime_sdk::provider::rfc3339(
            UNIX_EPOCH + Duration::from_millis(credential.expires as u64),
        )
    })
}

fn account(credential: &Credential) -> Account {
    Account {
        name: credential.label(),
        credentials: credential.saved(),
        expires_at: expires_at(credential),
    }
}

impl Provider for Kiro {
    async fn sign_in(&self, request: SignIn) -> Result<Step, RpcError> {
        let client = self.client(request.question.proxy.as_deref())?;
        match request.way.as_str() {
            "key" => Ok(self
                .sign_in_with_key(client.as_ref(), &request.inputs)
                .await?),
            "browser" => {
                let (pending, url) = auth::start(client, &self.installation).await?;
                let session = random_hex(24);
                let mut sessions = self.pending.lock().await;
                if sessions.len() >= 8 {
                    sessions.clear();
                }
                sessions.insert(session.clone(), pending);
                Ok(Step::Open {
                    url,
                    code: None,
                    session,
                })
            }
            way => Err(PluginError::unsupported(format!("Signing in by {way}")).into()),
        }
    }

    async fn sign_in_finish(&self, request: SignInFinish) -> Result<Step, RpcError> {
        let mut sessions = self.pending.lock().await;
        let pending = sessions
            .get_mut(&request.session)
            .ok_or_else(session::expired)?;
        let Some(result) = pending.poll().await else {
            return Ok(Step::Waiting);
        };
        sessions.remove(&request.session);
        Ok(Step::Account(account(&result?)))
    }

    async fn refresh(&self, question: Question) -> Result<Renewed, RpcError> {
        let client = self.client(question.proxy.as_deref())?;
        let saved = question.credentials.as_ref().ok_or_else(session::expired)?;
        let next = renew(client.as_ref(), &Credential::from_saved(saved)?, saved).await?;
        Ok(Renewed {
            credentials: next.saved(),
            expires_at: expires_at(&next),
        })
    }

    async fn models(&self, question: Question) -> Result<Vec<Model>, RpcError> {
        let client = self.client(question.proxy.as_deref())?;
        let (credential, _) = self.live(&question, client.as_ref()).await?;
        let listed =
            network::management(client.as_ref(), &credential, "List-Available-Models", None)
                .await?;
        let models = usage::models(&listed)?;
        let mut windows = self.windows.lock().await;
        for model in &models {
            if let Some(context) = model.context {
                windows.insert(model.id.clone(), context);
            }
        }
        Ok(models)
    }

    async fn usage(&self, question: Question) -> Result<Usage, RpcError> {
        let client = self.client(question.proxy.as_deref())?;
        let (credential, _) = self.live(&question, client.as_ref()).await?;
        let limits =
            network::management(client.as_ref(), &credential, "Get-Usage-Limits", None).await?;
        Ok(usage::parse(&limits)?)
    }

    async fn fetch(&self, mut request: Fetch) -> Result<FetchHead, RpcError> {
        self.check_address(&request.method, &request.url)?;
        let bytes = request.body.read_to_end(MAXIMUM_REQUEST_BYTES).await?;
        let body: Value = serde_json::from_slice(&bytes)
            .map_err(|_| PluginError::invalid("The Kiro request body is invalid JSON."))?;
        let model = session::text(body.get("model"), 256)
            .filter(|id| {
                id.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-:".contains(&b))
            })
            .ok_or_else(|| PluginError::invalid("The Kiro model ID is invalid."))?
            .to_owned();
        let client = self.client(request.question.proxy.as_deref())?;
        let (mut credential, mut renewed) = self.live(&request.question, client.as_ref()).await?;
        let budget = request::thinking(&body, &model, None);
        let profile = if credential.method == "apikey" {
            ""
        } else {
            &credential.profile
        };
        let (payload, names) = request::build(&body, &model, profile, budget)?;
        let mut response = self.send(client.as_ref(), &credential, &payload).await?;
        if response.status == 403 && credential.method != "apikey" {
            // Kiro turned the access away before it expired: renewed once
            let saved = request
                .question
                .credentials
                .as_ref()
                .ok_or_else(session::expired)?;
            let mut next = renew(client.as_ref(), &credential, saved).await?;
            if next.profile.is_empty() {
                next.profile = credential.profile.clone();
            }
            if next.access != credential.access {
                let (payload, _) = request::build(&body, &model, &next.profile, budget)?;
                response = self.send(client.as_ref(), &next, &payload).await?;
                credential = next;
                renewed = Some(credential.clone());
            }
        }
        let context = self.windows.lock().await.get(&model).copied().unwrap_or(0);
        let FetchResponse {
            status,
            headers,
            mut body,
        } = stream::response(
            response,
            model,
            body["stream"] == true,
            names,
            budget,
            context,
        )
        .await?;
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
            expires_at: renewed.as_ref().and_then(expires_at),
            credentials: renewed.map(|credential| credential.saved()),
        })
    }
}

/// Serves Kiro on stdin and stdout.
pub async fn serve() -> std::io::Result<()> {
    nora_plugin_runtime_sdk::provider::serve_provider(Kiro::default()).await
}

#[cfg(test)]
#[path = "../test/fixture.rs"]
mod fixture;
#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
