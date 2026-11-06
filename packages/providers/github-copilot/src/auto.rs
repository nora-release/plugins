use super::{copilot_client, refusals, visible};
use nora_ai::{HttpClient, HttpMethod, HttpRequest, Model, ResolvedProviderAuth};
use nora_provider::{definition::Definition, discovery::parsing, discovery::Kind};
use nora_provider_plugin_sdk::model_resolution::{ModelResolution, ResolveModelRequest};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::{sync::Mutex, time::Instant};

const MAX_ACCOUNTS: usize = 64;
const MAX_SELECTIONS: usize = 32;
const MAX_MODELS: usize = 4096;
const REUSE_FOR: Duration = Duration::from_secs(3600);

#[derive(Default)]
pub(super) struct Resolver {
    accounts: Mutex<BTreeMap<String, Arc<Mutex<Account>>>>,
    refused: Arc<refusals::Refusals>,
}

impl std::fmt::Debug for Resolver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CopilotAuto")
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct Account {
    current: Option<Selection>,
    issued: BTreeMap<String, Selection>,
    refused: Arc<refusals::Account>,
    models: BTreeMap<String, Model>,
    picks: Vec<String>,
    listed_at: Option<Instant>,
}

#[derive(Clone)]
struct Selection {
    id: String,
    model: Model,
    token: String,
    expires: Instant,
}

impl Selection {
    fn result(&self) -> PluginResult<ModelResolution> {
        let headers = if self.token.is_empty() {
            BTreeMap::new()
        } else {
            BTreeMap::from([("copilot-session-token".into(), self.token.clone())])
        };
        Ok(ModelResolution {
            model: serde_json::to_value(&self.model)
                .map_err(|_| invalid("Copilot returned an invalid selected model."))?,
            headers,
            selection_id: Some(self.id.clone()),
        })
    }
}

impl Resolver {
    pub(super) fn new(refused: Arc<refusals::Refusals>) -> Self {
        Self {
            accounts: Mutex::default(),
            refused,
        }
    }
    pub(super) async fn resolve(
        &self,
        definition: &Definition,
        request: ResolveModelRequest,
        auth: &ResolvedProviderAuth,
        client: &dyn HttpClient,
    ) -> PluginResult<Option<ModelResolution>> {
        let model: Model = serde_json::from_value(request.model.clone())
            .map_err(|_| invalid("The requested Copilot model is invalid."))?;
        if model.id != "auto" || !super::is_oauth(request.context.credentials.as_ref()) {
            return Ok(None);
        }
        nora_provider_plugin_sdk::require_subscription_accounts()?;
        let token = auth
            .token
            .as_deref()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| PluginError::authentication("Sign in to Copilot to use Auto."))?;
        let base = auth
            .base_url
            .as_deref()
            .unwrap_or(&definition.base_url)
            .trim_end_matches('/');
        let url =
            url::Url::parse(base).map_err(|_| invalid("The Copilot API address is invalid."))?;
        let local = url.scheme() == "http"
            && url
                .host_str()
                .and_then(|host| host.parse::<std::net::IpAddr>().ok())
                .is_some_and(|ip| ip.is_loopback())
            && url::Url::parse(&definition.base_url).ok().as_ref() == Some(&url);
        if (!local && url.scheme() != "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid("The Copilot API address is invalid."));
        }
        let key_token = auth
            .metadata
            .get("githubOAuthToken")
            .and_then(|value| value.as_str())
            .unwrap_or(token);
        let credentials = request
            .context
            .credentials
            .as_ref()
            .expect("OAuth checked above");
        let refused = self.refused.account(credentials);
        let key = format!(
            "{:x}",
            Sha256::digest(format!(
                "{}\0{base}\0{key_token}\0{}",
                request.context.account,
                refusals::identity(credentials)
            ))
        );
        let account = {
            let mut accounts = self.accounts.lock().await;
            if !accounts.contains_key(&key) && accounts.len() >= MAX_ACCOUNTS {
                if let Some(first) = accounts.keys().next().cloned() {
                    accounts.remove(&first);
                }
            }
            accounts
                .entry(key)
                .or_insert_with(|| {
                    Arc::new(Mutex::new(Account {
                        refused,
                        ..Default::default()
                    }))
                })
                .clone()
        };
        let mut state = account.lock().await;
        state
            .issued
            .retain(|_, selection| selection.expires + Duration::from_secs(300) > Instant::now());
        if request.force && !state.invalidate(&request) {
            return Ok(None);
        }
        if let Some(selected) = state.current.as_ref().filter(|selection| {
            selection.expires.saturating_duration_since(Instant::now()) > Duration::from_secs(120)
                && !state.refused.contains(&selection.model.id)
        }) {
            return selected.result().map(Some);
        }
        let credentials = request
            .context
            .credentials
            .as_ref()
            .expect("OAuth checked above");
        let prompt = request.prompt.trim();
        if prompt.len() > 128 * 1024 {
            return Err(invalid("The Copilot Auto prompt is too large."));
        }
        let prompt = if prompt.is_empty() { "hi" } else { prompt };
        let selected = tokio::time::timeout(
            Duration::from_secs(40),
            state.select(definition, credentials, auth, client, base, prompt),
        )
        .await
        .map_err(|_| PluginError::transport("Copilot Auto did not select a model in time."))??;
        state.current = Some(selected.clone());
        if state.issued.len() >= MAX_SELECTIONS {
            if let Some(first) = state.issued.keys().next().cloned() {
                state.issued.remove(&first);
            }
        }
        state.issued.insert(selected.id.clone(), selected.clone());
        selected.result().map(Some)
    }
}

impl Account {
    fn invalidate(&mut self, request: &ResolveModelRequest) -> bool {
        let Some(failure) = request
            .previous_failure
            .as_ref()
            .filter(|failure| failure.status == 400)
        else {
            return false;
        };
        let Some(previous) = failure
            .selection_id
            .as_deref()
            .and_then(|id| self.issued.get(id))
            .cloned()
        else {
            return false;
        };
        if failure.body.len() > 64 * 1024 {
            return false;
        }
        let body = failure.body.to_ascii_lowercase();
        let refused = refusals::model_refused(failure.body.as_bytes());
        let session_gone = !previous.token.is_empty() && body.contains("not available for session");
        if !refused && !session_gone {
            return false;
        }
        if refused {
            self.refused.remember(&previous.model.id);
        }
        if self.current.as_ref().is_some_and(|current| {
            if refused {
                current.model.id == previous.model.id
            } else {
                current.id == previous.id
            }
        }) {
            self.current = None;
        }
        true
    }

    async fn select(
        &mut self,
        definition: &Definition,
        credentials: &Value,
        auth: &ResolvedProviderAuth,
        client: &dyn HttpClient,
        base: &str,
        prompt: &str,
    ) -> PluginResult<Selection> {
        let auto = request_json(
            client,
            request(
                auth,
                base,
                "/auto",
                Some(json!({"prompt":prompt,"tier":"balance"})),
                Some("2026-08-01"),
            )?,
        )
        .await;
        if let Ok((status, body)) = auto {
            if (200..300).contains(&status) {
                if let Some((token, selected)) = selected(&body) {
                    if self
                        .refused
                        .contains(model_id(&selected).unwrap_or_default())
                    {
                        return self
                            .fallback(definition, credentials, auth, client, base)
                            .await;
                    }
                    let model = self.selected_model(definition, credentials, &selected, base)?;
                    return selection(&body, model, token, REUSE_FOR);
                }
            }
        }
        let (status, body) = request_json(
            client,
            request(
                auth,
                base,
                "/models/session",
                Some(json!({"auto_mode":{"model_hints":["auto"]}})),
                Some("2025-10-01"),
            )?,
        )
        .await?;
        if status == 404 {
            return self
                .fallback(definition, credentials, auth, client, base)
                .await;
        }
        if !(200..300).contains(&status) {
            return Err(PluginError::transport(format!(
                "Copilot Auto returned HTTP {status}."
            )));
        }
        let token = text(body.get("session_token"), 8192)
            .ok_or_else(|| invalid("Copilot Auto returned no session."))?;
        let mut choice = body
            .get("selected_model")
            .filter(|model| model_id(model).is_some())
            .cloned();
        if choice
            .as_ref()
            .and_then(model_id)
            .is_some_and(|id| self.refused.contains(id))
        {
            choice = None;
        }
        if choice.is_none() {
            self.list(definition, credentials, auth, client, base)
                .await?;
            let available = body
                .get("available_models")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            choice = available
                .iter()
                .filter(|entry| model_id(entry).is_some_and(|id| !self.refused.contains(id)))
                .find(|entry| model_id(entry).is_some_and(|id| self.models.contains_key(id)))
                .or_else(|| {
                    available
                        .iter()
                        .find(|entry| model_id(entry).is_some_and(|id| !self.refused.contains(id)))
                })
                .cloned();
            if choice.is_none() && !available.is_empty() {
                return self
                    .fallback(definition, credentials, auth, client, base)
                    .await;
            }
        }
        let choice = choice.ok_or_else(|| invalid("Copilot Auto selected no model."))?;
        if choice.get("supported_endpoints").is_none() {
            self.list(definition, credentials, auth, client, base)
                .await?;
        }
        let model = self.selected_model(definition, credentials, &choice, base)?;
        selection(&body, model, token, REUSE_FOR)
    }

    fn selected_model(
        &self,
        definition: &Definition,
        credentials: &Value,
        entry: &Value,
        base: &str,
    ) -> PluginResult<Model> {
        let id =
            model_id(entry).ok_or_else(|| invalid("Copilot Auto selected an invalid model."))?;
        let mut selected = if entry.is_object() {
            entry.clone()
        } else {
            json!({"id":id})
        };
        selected["model_picker_enabled"] = json!(true);
        selected["policy"] = json!({"state":"enabled"});
        if !selected["capabilities"].is_object() {
            selected["capabilities"] = json!({});
        }
        selected["capabilities"]["type"] = json!("chat");
        let mut model = parsing::model(
            &selected,
            Kind::OpenAI,
            definition,
            credentials,
            &self.models,
        )
        .ok_or_else(|| invalid("Copilot Auto selected a model without a supported HTTP API."))?;
        if selected.get("supported_endpoints").is_none() {
            if let Some(known) = self.models.get(id) {
                model.api = known.api.clone();
            }
        }
        model.base_url = base.into();
        Ok(model)
    }

    async fn list(
        &mut self,
        definition: &Definition,
        credentials: &Value,
        auth: &ResolvedProviderAuth,
        client: &dyn HttpClient,
        base: &str,
    ) -> PluginResult<()> {
        if self
            .listed_at
            .is_some_and(|at| at.elapsed() < Duration::from_secs(600))
        {
            return Ok(());
        }
        let (status, body) =
            request_json(client, request(auth, base, "/models", None, None)?).await?;
        if !(200..300).contains(&status) {
            return Err(PluginError::transport(format!(
                "Copilot models returned HTTP {status}."
            )));
        }
        let rows = body
            .get("data")
            .and_then(Value::as_array)
            .filter(|rows| rows.len() <= MAX_MODELS)
            .ok_or_else(|| invalid("Copilot returned an invalid model list."))?;
        let mut models = BTreeMap::new();
        let mut picks = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            let Some(id) = model_id(row) else { continue };
            if row.pointer("/capabilities/type").and_then(Value::as_str) != Some("chat") {
                continue;
            }
            if let Some(mut model) = self.selected_model(definition, credentials, row, base).ok() {
                model.base_url = base.into();
                models.insert(id.to_string(), model);
            }
            let enabled = row.pointer("/policy/state").and_then(Value::as_str) == Some("enabled")
                || row.get("policy").is_none() && row["model_picker_enabled"] == true;
            if visible(row, id) && enabled && models.contains_key(id) && !self.refused.contains(id)
            {
                let rank = if row["is_chat_fallback"] == true {
                    0
                } else if row["is_chat_default"] == true {
                    1
                } else if row.pointer("/billing/is_premium") == Some(&Value::Bool(false)) {
                    2
                } else {
                    3
                };
                picks.push((rank, index, id.to_string()));
            }
        }
        picks.sort_by_key(|(rank, index, _)| (*rank, *index));
        self.models = models;
        self.picks = picks.into_iter().map(|(_, _, id)| id).collect();
        self.listed_at = Some(Instant::now());
        Ok(())
    }

    async fn fallback(
        &mut self,
        definition: &Definition,
        credentials: &Value,
        auth: &ResolvedProviderAuth,
        client: &dyn HttpClient,
        base: &str,
    ) -> PluginResult<Selection> {
        self.list(definition, credentials, auth, client, base)
            .await?;
        let model = self
            .picks
            .iter()
            .filter(|id| !self.refused.contains(id))
            .find_map(|id| self.models.get(id))
            .cloned()
            .ok_or_else(|| {
                PluginError::transport(
                    "Copilot does not offer Auto or an available model this account may select.",
                )
            })?;
        selection(&json!({}), model, "", Duration::from_secs(600))
    }
}

fn request(
    auth: &ResolvedProviderAuth,
    base: &str,
    path: &str,
    body: Option<Value>,
    version: Option<&str>,
) -> PluginResult<HttpRequest> {
    let mut request = HttpRequest::new(
        if body.is_some() {
            HttpMethod::Post
        } else {
            HttpMethod::Get
        },
        format!("{base}{path}"),
    );
    request.headers = if auth
        .headers
        .get("copilot-integration-id")
        .map(String::as_str)
        == Some("copilot-developer-cli")
    {
        copilot_client::cli_headers()
    } else {
        copilot_client::api_headers()
    };
    request.headers.insert(
        "authorization".into(),
        format!("Bearer {}", auth.token.as_deref().unwrap_or_default()),
    );
    request
        .headers
        .insert("accept".into(), "application/json".into());
    if let Some(version) = version {
        request
            .headers
            .insert("x-github-api-version".into(), version.into());
    }
    if let Some(body) = body {
        request = request
            .with_json_body(&body)
            .map_err(|_| invalid("The Copilot Auto request is invalid."))?;
    }
    Ok(request)
}

async fn request_json(client: &dyn HttpClient, request: HttpRequest) -> PluginResult<(u16, Value)> {
    tokio::time::timeout(Duration::from_secs(15), async {
        let response = client.stream(request).await.map_err(|_| {
            PluginError::transport("The Copilot Auto service could not be reached.")
        })?;
        let status = response.status;
        let body = nora_ai::collect_http_body_text_limited(response.body, 4 * 1024 * 1024)
            .await
            .map_err(|_| {
                PluginError::transport("The Copilot Auto response was incomplete or too large.")
            })?;
        let value = match serde_json::from_str(&body) {
            Ok(value) => value,
            Err(_) if !(200..300).contains(&status) => Value::Null,
            Err(_) => return Err(invalid("Copilot Auto returned invalid JSON.")),
        };
        Ok((status, value))
    })
    .await
    .map_err(|_| PluginError::transport("The Copilot Auto request timed out."))?
}

fn selected(body: &Value) -> Option<(&str, Value)> {
    let token = text(body.get("session_token"), 8192)?;
    let selected = body.get("selected_model")?;
    model_id(selected)?;
    Some((token, selected.clone()))
}

fn selection(body: &Value, model: Model, token: &str, reuse: Duration) -> PluginResult<Selection> {
    let now = nora_ai::oauth::now_millis() / 1000;
    let expires = body
        .get("expires_at")
        .and_then(Value::as_i64)
        .filter(|expires| *expires > 0)
        .map(|expires| expires.saturating_sub(now));
    if expires.is_some_and(|seconds| seconds <= 0) {
        return Err(invalid("Copilot Auto returned an expired session."));
    }
    let lifetime = expires
        .map(|seconds| Duration::from_secs(seconds as u64))
        .unwrap_or(reuse)
        .min(reuse);
    Ok(Selection {
        id: nora_ai::oauth_pkce::random_hex(24),
        model,
        token: token.into(),
        expires: Instant::now() + lifetime,
    })
}

fn model_id(model: &Value) -> Option<&str> {
    text(
        if model.is_string() {
            Some(model)
        } else {
            model.get("id")
        },
        512,
    )
    .filter(|id| *id != "auto" && !id.chars().any(char::is_whitespace))
}

fn text(value: Option<&Value>, max: usize) -> Option<&str> {
    value.and_then(Value::as_str).filter(|value| {
        !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
    })
}

fn invalid(message: &str) -> PluginError {
    PluginError::transport(message)
}

#[cfg(test)]
#[path = "../test/auto.test.rs"]
mod tests;
