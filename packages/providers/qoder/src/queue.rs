use crate::*;
use std::collections::VecDeque;
use tokio::time::Instant;

const PATH: &str = "/algo/api/v2/service/ask/queue/status";
pub(crate) const BUDGET: Duration = Duration::from_secs(240);
pub(crate) const RECOVERIES: usize = 10;

#[derive(Clone, Debug, Default)]
pub(crate) struct State {
    queued: Option<bool>,
    available: Option<bool>,
    model: Option<String>,
    kind: Option<String>,
    after: Option<f64>,
}

fn flag(value: &Value) -> Option<bool> {
    value.as_bool().or_else(|| match value.as_str() {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    })
}

pub(crate) fn state(value: &Value) -> Option<State> {
    let mut pending = VecDeque::from([value.clone()]);
    for _ in 0..128 {
        let value = pending.pop_front()?;
        if !value.is_object() {
            continue;
        }
        let queued = value.get("isQueued").and_then(flag);
        let available = value.get("serviceAvailable").and_then(flag);
        if queued.is_some() || available.is_some() {
            return Some(State {
                queued,
                available,
                model: value
                    .get("modelKey")
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned),
                kind: value
                    .get("queueType")
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned),
                after: value.get("retryAfterSeconds").and_then(number),
            });
        }
        for key in ["data", "result", "message", "body", "queue"] {
            if let Some(value) = value.get(key) {
                if value.is_object() {
                    pending.push_back(value.clone());
                } else if let Some(parsed) = value
                    .as_str()
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                {
                    pending.push_back(parsed);
                }
            }
        }
    }
    None
}

impl State {
    pub(crate) fn busy(&self) -> bool {
        self.queued == Some(true) || self.available == Some(false)
    }
    pub(crate) fn retry_after(&self) -> u64 {
        self.after
            .filter(|n| *n > 0.0)
            .map(|n| n.ceil() as u64)
            .unwrap_or(30)
    }
    pub(crate) fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }
}

fn pause(seconds: Option<f64>) -> Duration {
    Duration::from_secs_f64(
        seconds
            .filter(|n| n.is_finite())
            .unwrap_or(30.0)
            .clamp(0.5, 30.0),
    )
}

async fn sleep(seconds: Option<f64>, deadline: Instant) {
    tokio::time::sleep_until((Instant::now() + pause(seconds)).min(deadline)).await;
}

enum Status {
    State(State),
    End,
    Failed,
}

async fn status(
    client: &dyn HttpClient,
    site: Site,
    api: &str,
    credentials: &Value,
    body: &Value,
    model: &str,
    previous: &State,
    deadline: Instant,
) -> Status {
    let Ok(mut url) = url::Url::parse(&format!("{api}{PATH}")) else {
        return Status::End;
    };
    url.query_pairs_mut()
        .append_pair("requestSetId", text(body, "request_set_id"))
        .append_pair("modelKey", previous.model.as_deref().unwrap_or(model));
    if let Some(kind) = &previous.kind {
        url.query_pairs_mut().append_pair("queueType", kind);
    }
    let Ok(mut headers) = codec::headers_for(site, url.as_str(), credentials, "") else {
        return Status::Failed;
    };
    headers.remove("content-type");
    headers.insert("x-request-id".into(), text(body, "request_id").into());
    headers.insert("x-session-id".into(), text(body, "session_id").into());
    let request = HttpRequest {
        method: HttpMethod::Get,
        url: url.to_string(),
        headers,
        body: None,
    };
    let result = tokio::time::timeout_at(
        deadline.min(Instant::now() + Duration::from_secs(30)),
        async {
            let response = client.stream(request).await.ok()?;
            if response.status == 404 {
                return Some(Status::End);
            }
            if !(200..300).contains(&response.status) {
                return Some(Status::Failed);
            }
            let body = nora_ai::collect_http_body_text_limited(response.body, 1024 * 1024)
                .await
                .ok()?;
            let next = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|value| state(&value));
            Some(next.map(Status::State).unwrap_or(Status::End))
        },
    )
    .await;
    match result {
        Ok(Some(result)) => result,
        _ => Status::Failed,
    }
}

pub(crate) async fn wait(
    client: &dyn HttpClient,
    site: Site,
    api: &str,
    credentials: &Value,
    body: &Value,
    model: &str,
    first: State,
    deadline: Instant,
) -> bool {
    let mut state = first;
    let mut asked = false;
    let mut failed = 0;
    loop {
        if Instant::now() >= deadline {
            return false;
        }
        if asked {
            sleep(state.after, deadline).await;
        }
        asked = true;
        if Instant::now() >= deadline {
            return false;
        }
        match status(
            client,
            site,
            api,
            credentials,
            body,
            model,
            &state,
            deadline,
        )
        .await
        {
            Status::End => return false,
            Status::Failed => {
                failed += 1;
                if failed >= 3 {
                    return false;
                }
                state.after = None;
            }
            Status::State(mut next) => {
                failed = 0;
                if next.model.is_none() {
                    next.model = state.model.take();
                }
                if next.kind.is_none() {
                    next.kind = state.kind.take();
                }
                if next.queued == Some(false) {
                    if next.after.is_some_and(|n| n > 0.0) {
                        sleep(next.after, deadline).await;
                    }
                    return Instant::now() < deadline;
                }
                if !next.busy() {
                    return false;
                }
                state = next;
            }
        }
    }
}

#[cfg(test)]
#[path = "../test/queue.test.rs"]
mod tests;
