use crate::provider::*;
use crate::sign;
use nora_ai::{HttpClient, HttpMethod, HttpRequest, HttpStreamingResponse};
use nora_provider_plugin_sdk::{FetchRequest, FetchResponse};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

#[path = "stream.rs"]
mod stream;

pub(super) async fn fetch(
    provider: &CodeArts,
    request: FetchRequest,
) -> PluginResult<FetchResponse> {
    if request.method != "POST" || request.url != format!("{BASE}/chat/completions") {
        return Err(invalid("CodeArts only accepts its official chat endpoint."));
    }
    let credentials = decode(request.credentials.as_ref().ok_or_else(signed_out)?)?;
    let client = provider.client(request.network_proxy.as_ref())?;
    let bytes = request
        .decode_body()?
        .ok_or_else(|| invalid("CodeArts needs a chat request body."))?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err(invalid("The CodeArts chat request exceeds 32 MiB."));
    }
    let mut body: Value = serde_json::from_slice(&bytes)
        .map_err(|_| invalid("The CodeArts chat request is invalid JSON."))?;
    let (fold, mut injected) = prepare(&mut body)?;
    let model = text(&body, "model").to_owned();
    let account = provider.account(&credentials).await?;
    // Metadata is account-local even when model discovery and chat run in
    // different native processes. A first chat reads its own supplier list.
    let _ = crate::models::ensure(provider, client.as_ref(), &credentials).await;
    let mut benefit = {
        let state = account.models.lock().await;
        crate::models::benefit_for(&state, &model)
    };
    let permit = account
        .gate
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| invalid("The CodeArts concurrency gate stopped."))?;
    let started = tokio::time::Instant::now();
    let mut session_attempt = 0;
    let mut queue_deadline = None;
    loop {
        let bytes = body.to_string().into_bytes();
        let extra = if benefit {
            BTreeMap::from([("maas_type".into(), "benefit".into())])
        } else {
            BTreeMap::new()
        };
        let mut headers = sign::huawei(
            &credentials,
            "POST",
            &request.url,
            &bytes,
            extra,
            &chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string(),
        )?;
        session_headers(&mut headers, &credentials)?;
        let response = send(
            client.as_ref(),
            HttpRequest {
                method: HttpMethod::Post,
                url: request.url.clone(),
                headers,
                body: Some(bytes),
            },
            Duration::from_secs(120),
        )
        .await?;
        if response.status == 429
            && header(&response.headers, "queue-status")
                .is_some_and(|status| status.eq_ignore_ascii_case("queued"))
        {
            if let Some(trace) = header(&response.headers, "trace-id")
                .filter(|trace| !trace.is_empty() && trace.len() <= 512)
                .map(str::to_owned)
            {
                let after = header(&response.headers, "retry-after")
                    .and_then(|value| value.parse::<f64>().ok())
                    .filter(|value| value.is_finite() && *value >= 0.0)
                    .unwrap_or(5.0);
                drop(response);
                let deadline = *queue_deadline
                    .get_or_insert_with(|| tokio::time::Instant::now() + Duration::from_secs(300));
                if wait_in_queue(client.clone(), credentials.clone(), trace, after, deadline)
                    .await?
                {
                    continue;
                }
                return Ok(error_response(
                    429,
                    "queue_timeout",
                    "The CodeArts queue wait expired or the request left the queue.",
                ));
            }
        }
        let status = response.status;
        let (mut response, code, message) = if (200..300).contains(&status) {
            let head = stream::peek(response).await?;
            if head.code.is_none() {
                return if fold {
                    stream::fold(head.response, head.bytes, permit).await
                } else {
                    Ok(stream::replay(head.response, head.bytes, permit))
                };
            }
            (head.response, head.code.unwrap(), head.message)
        } else {
            let (status, headers, bytes) = crate::provider::body(response, 2 * 1024 * 1024).await?;
            let text = String::from_utf8_lossy(&bytes);
            let (code, message) = error_parts(&text);
            let response = HttpStreamingResponse {
                status,
                headers,
                body: Box::pin(futures::stream::once(async move { Ok(bytes.into()) })),
            };
            (response, code, message)
        };
        let phrase = format!("{code} {message}");
        if status == 401 || stale(&phrase) {
            // Nora serializes token refreshes across its native processes and
            // persists the rotated pair before retrying. Spending it here too
            // would race the host's background renewal of one-use STS tokens.
            return Ok(error_response(
                401,
                "authentication_error",
                "CodeArts requires renewed Huawei Cloud credentials.",
            ));
        }
        if status == 400 && injected && phrase.to_ascii_lowercase().contains("thinking") {
            body.as_object_mut().unwrap().remove("thinking");
            injected = false;
            continue;
        }
        if benefit
            && (code == "InferHub.4004.200"
                || code == "InferHub.4005.200"
                || phrase.to_ascii_lowercase().contains("benefit not found"))
        {
            let mut state = account.models.lock().await;
            if state
                .regular
                .as_ref()
                .is_none_or(|ids| ids.contains(&model))
            {
                if state.refused.len() < 4096 {
                    state.refused.insert(model.clone());
                }
                benefit = false;
                continue;
            }
        }
        if code == "TM.00001041" || phrase.contains("TM.00001041") {
            let delay =
                Duration::from_millis([2000, 4000, 6000, 8000, 10000][session_attempt.min(4)]);
            session_attempt += 1;
            if started.elapsed() + delay <= Duration::from_secs(150) {
                drop(response);
                tokio::time::sleep(delay).await;
                continue;
            }
            return Ok(error_response(
                429,
                "TM.00001041",
                if message.is_empty() {
                    "The CodeArts account has reached its concurrent session limit."
                } else {
                    &message
                },
            ));
        }
        if (200..300).contains(&status) {
            return Ok(error_response(
                error_status(&code),
                &code,
                if message.is_empty() {
                    "CodeArts returned an error inside its response stream."
                } else {
                    &message
                },
            ));
        }
        response.headers.retain(|name, _| {
            !matches!(
                name.to_ascii_lowercase().as_str(),
                "content-length" | "content-encoding"
            )
        });
        response.headers.insert(
            "x-magpie-sign-in".into(),
            if status == 403 { "expired" } else { "kept" }.into(),
        );
        return Ok(stream::replay(response, Vec::new(), permit));
    }
}

pub(super) fn prepare(body: &mut Value) -> PluginResult<(bool, bool)> {
    let Some(model) = body
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .map(str::to_owned)
    else {
        return Err(invalid("CodeArts needs a model ID."));
    };
    if !body.get("messages").is_some_and(Value::is_array) {
        return Err(invalid("CodeArts needs chat messages."));
    }
    body["model"] = json!(model.strip_prefix("codearts/").unwrap_or(&model));
    let fold = body.get("stream").and_then(Value::as_bool) != Some(true);
    let injected = body.get("thinking").is_none();
    if injected {
        body["thinking"] = json!({"type":"enabled"});
    }
    for key in ["max_tokens", "max_completion_tokens"] {
        if body
            .get(key)
            .and_then(number)
            .is_some_and(|value| value > 65536.0)
        {
            body[key] = json!(65536);
        }
    }
    body["stream"] = json!(true);
    Ok((fold, injected))
}
fn session_headers(
    headers: &mut BTreeMap<String, String>,
    credentials: &Value,
) -> PluginResult<()> {
    let (session, chat) = session(credentials);
    headers.extend([
        ("chat-id".into(), chat),
        ("session-id".into(), session.clone()),
        ("x-ot-session-id".into(), session.clone()),
        ("user-session-id".into(), session),
        ("x-ot-parent-session-id".into(), String::new()),
        ("x-ot-trace-id".into(), sign::id(16)?),
        ("x-ot-span-id".into(), sign::id(8)?),
        ("x-snap-traceid".into(), sign::id(16)?),
        ("lang".into(), "en".into()),
    ]);
    Ok(())
}
pub(super) fn error_parts(text: &str) -> (String, String) {
    for value in std::iter::once(text).chain(
        text.lines()
            .filter_map(|line| line.trim().strip_prefix("data:").map(str::trim)),
    ) {
        if let Ok(value) = serde_json::from_str::<Value>(value) {
            for value in std::iter::once(&value).chain(value.get("error")) {
                let code = value
                    .get("error_code")
                    .or_else(|| value.get("code"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if !code.is_empty() {
                    let message = value
                        .get("error_msg")
                        .or_else(|| value.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    return (code.into(), message.into());
                }
            }
        }
    }
    (String::new(), String::new())
}
fn stale(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    [
        "apig.0301",
        "apig.0307",
        "apig.0602",
        "security token has expired",
        "verify ak sk signature fail",
    ]
    .iter()
    .any(|word| text.contains(word))
}
pub(super) fn error_status(code: &str) -> u16 {
    if code.contains("4291")
        || code
            .split('.')
            .any(|piece| piece == "429" || piece.starts_with("8111"))
    {
        429
    } else if code
        .split('.')
        .any(|piece| matches!(piece, "400" | "401" | "402" | "403" | "404"))
        || code.starts_with("InferHub.400")
    {
        400
    } else {
        502
    }
}
pub(super) fn error_response(status: u16, code: &str, message: &str) -> FetchResponse {
    FetchResponse::bytes(status,BTreeMap::from([("content-type".into(),"application/json".into())]),json!({"error":{"message":message,"code":code,"type":if status==429{"rate_limit_error"}else if status==401{"authentication_error"}else{"provider_error"}}}).to_string().into_bytes())
}
fn header<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

struct Queue {
    client: Arc<dyn HttpClient>,
    credentials: Value,
    trace: String,
    session: String,
    active: bool,
    meter: nora_provider_plugin_sdk::RequestMeterContext,
}
impl Drop for Queue {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let client = self.client.clone();
        let credentials = self.credentials.clone();
        let trace = self.trace.clone();
        let session = self.session.clone();
        let meter = self.meter.clone();
        tokio::spawn(async move {
            let _ = tokio::time::timeout(
                Duration::from_secs(3),
                meter.cleanup_scope(queue_call(
                    client.as_ref(),
                    &credentials,
                    HttpMethod::Delete,
                    &trace,
                    &session,
                )),
            )
            .await;
        });
    }
}
async fn wait_in_queue(
    client: Arc<dyn HttpClient>,
    credentials: Value,
    trace: String,
    mut after: f64,
    deadline: tokio::time::Instant,
) -> PluginResult<bool> {
    let mut guard = Queue {
        client,
        session: session(&credentials).0,
        credentials,
        trace,
        active: true,
        meter: nora_provider_plugin_sdk::RequestMeterContext::capture(),
    };
    let mut failures = 0;
    loop {
        let delay = Duration::from_secs_f64(after.clamp(0.0, 300.0));
        if tokio::time::Instant::now() + delay > deadline {
            return Ok(false);
        }
        tokio::time::sleep(delay).await;
        let polled = tokio::time::timeout_at(
            deadline,
            queue_call(
                guard.client.as_ref(),
                &guard.credentials,
                HttpMethod::Get,
                &guard.trace,
                &guard.session,
            ),
        )
        .await;
        let Ok(polled) = polled else { return Ok(false) };
        match polled {
            Ok(value) => match text(&value, "status") {
                "promoted" => {
                    guard.active = false;
                    return Ok(true);
                }
                "not_found" | "error" => return Ok(false),
                _ => {
                    if let Some(value) = value
                        .get("retry_after")
                        .and_then(number)
                        .filter(|value| *value >= 0.0)
                    {
                        after = value;
                    }
                }
            },
            Err(_) => {
                failures += 1;
                if failures >= 3 {
                    return Ok(false);
                }
            }
        }
    }
}
async fn queue_call(
    client: &dyn HttpClient,
    credentials: &Value,
    method: HttpMethod,
    trace: &str,
    session: &str,
) -> PluginResult<Value> {
    let url = format!(
        "{SNAP}/api/v1/concurrency/trace-queues/{}",
        sign::encode(trace)
    );
    let mut headers = sign::huawei(
        credentials,
        method.as_str(),
        &url,
        b"",
        BTreeMap::new(),
        &chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string(),
    )?;
    headers.insert("x-ot-session-id".into(), session.into());
    let response = send(
        client,
        HttpRequest {
            method,
            url,
            headers,
            body: None,
        },
        Duration::from_secs(15),
    )
    .await?;
    if response.status == 204 {
        return Ok(json!({"status":"not_found"}));
    }
    json_body(response).await
}

#[cfg(test)]
#[path = "../test/gateway.test.rs"]
mod tests;
