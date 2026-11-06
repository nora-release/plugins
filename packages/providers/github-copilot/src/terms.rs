use nora_ai::{HttpClient, HttpMethod, HttpRequest};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};
use tokio::{sync::Mutex, time::Instant};

#[derive(Default)]
pub(super) struct Approvals {
    accounts: Mutex<BTreeMap<String, Arc<Mutex<Account>>>>,
}

impl std::fmt::Debug for Approvals {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CopilotTerms")
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct Account {
    waiting: BTreeSet<String>,
    listed: Option<Instant>,
}

impl Approvals {
    pub(super) async fn before_send(&self, request: &HttpRequest, client: &dyn HttpClient) {
        let Some(body) = request
            .body
            .as_deref()
            .and_then(|body| serde_json::from_slice::<Value>(body).ok())
        else {
            return;
        };
        let Some(model) = body
            .get("model")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && *id != "auto" && id.len() <= 512)
        else {
            return;
        };
        let Ok(mut base) = url::Url::parse(&request.url) else {
            return;
        };
        let Some(path) = [
            "/v1/messages",
            "/messages",
            "/chat/completions",
            "/responses",
        ]
        .into_iter()
        .find_map(|suffix| base.path().strip_suffix(suffix).map(str::to_string)) else {
            return;
        };
        let Some(token) = request
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("authorization"))
            .map(|(_, token)| token)
        else {
            return;
        };
        if nora_provider_plugin_sdk::require_subscription_accounts().is_err() {
            return;
        }
        base.set_path(&format!("{}/", path.trim_end_matches('/')));
        base.set_query(None);
        base.set_fragment(None);
        let key = format!("{:x}", Sha256::digest(format!("{base}\0{token}")));
        let account = {
            let mut accounts = self.accounts.lock().await;
            if !accounts.contains_key(&key) && accounts.len() >= 64 {
                if let Some(first) = accounts.keys().next().cloned() {
                    accounts.remove(&first);
                }
            }
            accounts.entry(key).or_default().clone()
        };
        let mut account = account.lock().await;
        let headers = request
            .headers
            .iter()
            .filter(|(key, _)| {
                matches!(
                    key.to_ascii_lowercase().as_str(),
                    "authorization"
                        | "editor-version"
                        | "editor-plugin-version"
                        | "copilot-integration-id"
                        | "user-agent"
                        | "x-github-api-version"
                )
            })
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        if account
            .listed
            .is_none_or(|at| at.elapsed() > Duration::from_secs(600))
        {
            let Ok(models) = base.join("models") else {
                return;
            };
            let outgoing = HttpRequest {
                method: HttpMethod::Get,
                url: models.into(),
                headers: headers.clone(),
                body: None,
            };
            let Some((status, body)) = json(client, outgoing).await else {
                return;
            };
            if !(200..300).contains(&status) {
                return;
            }
            let Some(models) = body
                .get("data")
                .and_then(Value::as_array)
                .filter(|models| models.len() <= 4096)
            else {
                return;
            };
            account.waiting = models
                .iter()
                .filter_map(|model| {
                    let id = model
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty() && id.len() <= 512)?;
                    let terms = model
                        .pointer("/policy/terms")
                        .and_then(Value::as_str)
                        .filter(|terms| !terms.is_empty());
                    (terms.is_some()
                        && model.pointer("/policy/state").and_then(Value::as_str)
                            != Some("enabled")
                        && super::visible(model, id))
                    .then(|| id.to_string())
                })
                .collect();
            account.listed = Some(Instant::now());
        }
        if !account.waiting.contains(model) {
            return;
        }
        let mut url = base;
        let Ok(mut segments) = url.path_segments_mut() else {
            return;
        };
        segments
            .pop_if_empty()
            .push("models")
            .push(model)
            .push("policy");
        drop(segments);
        let outgoing = HttpRequest {
            method: HttpMethod::Post,
            url: url.into(),
            headers,
            body: None,
        };
        let Ok(outgoing) = outgoing.with_json_body(&json!({"state":"enabled"})) else {
            return;
        };
        if json(client, outgoing)
            .await
            .is_some_and(|(status, _)| (200..300).contains(&status))
        {
            account.waiting.remove(model);
        }
    }
}

async fn json(client: &dyn HttpClient, request: HttpRequest) -> Option<(u16, Value)> {
    tokio::time::timeout(Duration::from_secs(15), async {
        let response = client.stream(request).await.ok()?;
        let status = response.status;
        let body = nora_ai::collect_http_body_text_limited(response.body, 4 * 1024 * 1024)
            .await
            .ok()?;
        Some((status, serde_json::from_str(&body).unwrap_or(Value::Null)))
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
#[path = "../test/terms.test.rs"]
mod tests;
