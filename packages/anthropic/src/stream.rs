use super::{cli, mcp, prompt, session::Run};
use async_trait::async_trait;
use futures::{stream, StreamExt};
use nora_provider_plugin_sdk::{FetchResponse, PluginError, PluginResult};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};
use tokio::sync::OwnedMutexGuard;
#[async_trait]
pub(crate) trait Persistence: Send + Sync {
    async fn save(&self, value: Value) -> PluginResult<()>;
}
pub(crate) struct Host;
#[async_trait]
impl Persistence for Host {
    async fn save(&self, value: Value) -> PluginResult<()> {
        nora_provider_plugin_sdk::save_credentials(value).await
    }
}
pub(crate) fn status(value: &Value) -> u16 {
    let text = value.to_string().to_ascii_lowercase();
    if let Some(status) = value["api_error_status"]
        .as_u64()
        .filter(|status| (400..=599).contains(status))
    {
        return status as u16;
    }
    if [
        "rate_limit",
        "rate limit",
        "quota",
        "usage limit",
        "hit your limit",
        "too many requests",
    ]
    .iter()
    .any(|word| text.contains(word))
    {
        429
    } else if [
        "authenticat",
        "not logged",
        "not signed",
        "oauth session expired",
        "revoked",
        "sign in again",
        "invalid token",
    ]
    .iter()
    .any(|word| text.contains(word))
    {
        401
    } else if text.contains("overload") {
        503
    } else if text.contains("too long") || text.contains("context_length") {
        400
    } else {
        502
    }
}
pub(crate) fn failure(status: u16) -> FetchResponse {
    let kind = match status {
        401 | 403 => "authentication_error",
        429 => "rate_limit_error",
        400 => "invalid_request_error",
        503 => "overloaded_error",
        _ => "api_error",
    };
    let message = match status {
        401 | 403 => "Claude Code could not use this account. Sign in again.",
        429 => "Claude subscription usage or rate limit reached.",
        400 => "The request is too long or invalid for Claude Code.",
        503 => "Claude is temporarily overloaded.",
        _ => "Claude Code stopped before completing the reply.",
    };
    FetchResponse::bytes(
        status,
        BTreeMap::from([("content-type".into(), "application/json".into())]),
        serde_json::to_vec(&json!({"type":"error","error":{"type":kind,"message":message}}))
            .unwrap(),
    )
}
pub(crate) async fn synchronize(
    account: &mut cli::Account,
    persistence: &dyn Persistence,
) -> PluginResult<()> {
    if account.home.is_own() {
        return Ok(());
    }
    let oauth = cli::credentials(&account.home).await?;
    let access = cli::text(oauth.get("accessToken"), 64 * 1024).ok_or_else(cli::error)?;
    let refresh = cli::text(oauth.get("refreshToken"), 64 * 1024).ok_or_else(cli::error)?;
    if account.saved["access"] == access && account.saved["refresh"] == refresh {
        return Ok(());
    }
    let mut next = account.clone();
    next.saved["access"] = json!(access);
    next.saved["refresh"] = json!(refresh);
    next.saved["expires"] = oauth["expiresAt"].clone();
    persistence.save(next.saved()).await?;
    *account = next;
    Ok(())
}
struct State {
    run: OwnedMutexGuard<Run>,
    persistence: Arc<dyn Persistence>,
    queued: VecDeque<Vec<u8>>,
    stream: bool,
    done: bool,
    complete: bool,
    visible: bool,
    failed: Option<u16>,
    message: Value,
    content: BTreeMap<usize, Value>,
    native_ids: std::collections::BTreeSet<String>,
    stopped: Option<String>,
    usage: Value,
    pending_stop: bool,
}
impl Drop for State {
    fn drop(&mut self) {
        if !self.complete {
            self.run.cancel();
        }
    }
}
impl State {
    fn send(&mut self, value: Value) {
        if self.stream {
            let event = value["type"].as_str().unwrap_or("message");
            self.queued
                .push_back(format!("event: {event}\ndata: {value}\n\n").into_bytes());
        }
    }
    fn fail(&mut self, status: u16) {
        self.failed = Some(status);
        self.done = true;
        self.run.cancel();
        self.queued.clear();
        if self.visible {
            self.queued.push_back(format!("event: error\ndata: {}\n\n",json!({"type":"error","error":{"type":if status==429{"rate_limit_error"}else{"api_error"},"message":if status==429{"Claude subscription usage or rate limit reached."}else{"Claude Code stopped before completing the reply."}}})).into_bytes());
        }
    }
    async fn finish(&mut self, tool: bool) -> PluginResult<()> {
        if self.complete {
            return Ok(());
        }
        synchronize(&mut self.run.account, self.persistence.as_ref()).await?;
        let content = self.content.values().cloned().collect::<Vec<_>>();
        if tool {
            let calls = content
                .iter()
                .filter(|part| part["type"] == "tool_use")
                .filter_map(|part| part["id"].as_str().map(str::to_owned))
                .collect::<std::collections::BTreeSet<_>>();
            if calls.is_empty() {
                return Err(PluginError::transport(
                    "Claude Code ended a tool turn without tool calls.",
                ));
            }
            self.run.parked = calls;
        }
        self.run
            .told
            .push(json!({"role":"assistant","content":content}));
        self.run.idle = std::time::Instant::now();
        self.complete = true;
        self.done = true;
        self.send(json!({"type":"message_stop"}));
        if !self.stream {
            let mut message = self.message.clone();
            if !message.is_object() {
                message = json!({"id":format!("msg_{}",cli::random()),"model":"claude","type":"message","role":"assistant"});
            }
            message["content"] = json!(content);
            message["usage"] = self.usage.clone();
            message["stop_reason"] = json!(self.stopped.as_deref().unwrap_or("end_turn"));
            message["stop_sequence"] = Value::Null;
            self.queued
                .push_back(serde_json::to_vec(&message).map_err(|_| cli::error())?);
        }
        Ok(())
    }
    async fn native(&mut self, mut event: Value) -> PluginResult<()> {
        match event["type"].as_str().unwrap_or_default() {
            "message_start" => {
                let id = cli::text(event.pointer("/message/id"), 256).ok_or_else(|| {
                    PluginError::transport("Claude Code returned a message without an identifier.")
                })?;
                if !self.native_ids.insert(id.into()) {
                    return Ok(());
                }
                if !self.message.is_null() {
                    return Err(PluginError::transport(
                        "Claude Code started a second reply before completing the first.",
                    ));
                }
                self.message = event["message"].clone();
                self.usage = event["message"]
                    .get("usage")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
            }
            "content_block_start" => {
                let index = event["index"]
                    .as_u64()
                    .filter(|index| *index < 10000)
                    .ok_or_else(cli::error)? as usize;
                let mut block = event["content_block"].clone();
                if block["type"] == "tool_use" {
                    let wire = cli::text(block.get("name"), 256)
                        .ok_or_else(cli::error)?
                        .to_owned();
                    let short = wire.strip_prefix(mcp::PREFIX).ok_or_else(|| {
                        PluginError::transport(
                            "Claude Code attempted a tool Nora did not advertise.",
                        )
                    })?;
                    let name = {
                        let state = self.run.bridge.state.lock().await;
                        state.names.get(short).cloned()
                    }
                    .ok_or_else(|| {
                        PluginError::transport(
                            "Claude Code attempted a tool Nora did not advertise.",
                        )
                    })?;
                    block["name"] = json!(name);
                    event["content_block"] = block.clone();
                    block["_wireName"] = json!(wire);
                    block["_arguments"] = json!("");
                    self.visible = true;
                }
                if self.content.insert(index, block).is_some() {
                    return Err(PluginError::transport("Claude Code reused a reply block."));
                }
            }
            "content_block_delta" => {
                let index = event["index"].as_u64().ok_or_else(cli::error)? as usize;
                let block = self.content.get_mut(&index).ok_or_else(|| {
                    PluginError::transport("Claude Code sent a delta without its reply block.")
                })?;
                let delta = &event["delta"];
                let key = match delta["type"].as_str() {
                    Some("text_delta") => Some(("text", "text")),
                    Some("thinking_delta") => Some(("thinking", "thinking")),
                    Some("signature_delta") => Some(("signature", "signature")),
                    Some("input_json_delta") => Some(("_arguments", "partial_json")),
                    _ => None,
                };
                if let Some((key, field)) = key {
                    let text = delta[field].as_str().ok_or_else(cli::error)?;
                    let old = block[key].as_str().unwrap_or_default();
                    if old.len().saturating_add(text.len()) > 32 * 1024 * 1024 {
                        return Err(PluginError::transport(
                            "The Claude reply block is too large.",
                        ));
                    }
                    block[key] = json!(format!("{old}{text}"));
                    if !text.is_empty() {
                        self.visible = true;
                    }
                }
            }
            "content_block_stop" => {
                let index = event["index"].as_u64().ok_or_else(cli::error)? as usize;
                let block = self.content.get_mut(&index).ok_or_else(cli::error)?;
                if block["type"] == "tool_use" {
                    let input = block["_arguments"].as_str().unwrap_or_default();
                    let input = if input.is_empty() {
                        block["input"].clone()
                    } else {
                        serde_json::from_str(input).map_err(|_| {
                            PluginError::transport("Claude Code returned invalid tool arguments.")
                        })?
                    };
                    if !input.is_object() {
                        return Err(PluginError::transport(
                            "Claude Code returned invalid tool arguments.",
                        ));
                    }
                    let id = cli::text(block.get("id"), 256)
                        .ok_or_else(cli::error)?
                        .to_owned();
                    let name = block["_wireName"]
                        .as_str()
                        .ok_or_else(cli::error)?
                        .to_owned();
                    let restored = self.run.bridge.declare(&id, &name, input.clone()).await?;
                    block["input"] = input;
                    block["name"] = json!(restored);
                    block.as_object_mut().unwrap().remove("_wireName");
                    block.as_object_mut().unwrap().remove("_arguments");
                }
            }
            "message_delta" => {
                if let Some(reason) = event.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stopped = Some(reason.into());
                }
                for (key, value) in event["usage"].as_object().into_iter().flatten() {
                    if !value.is_null() {
                        self.usage[key] = value.clone();
                    }
                }
                event["usage"] = self.usage.clone();
            }
            "message_stop" => {
                if self.stopped.as_deref() == Some("tool_use") {
                    self.finish(true).await?;
                } else {
                    self.pending_stop = true;
                }
                return Ok(());
            }
            "error" => {
                self.fail(status(&event));
                return Ok(());
            }
            _ => {}
        }
        self.send(event);
        Ok(())
    }
    async fn outer(&mut self, value: Value) -> PluginResult<()> {
        match value["type"].as_str().unwrap_or_default() {
            "stream_event" => self.native(value["event"].clone()).await?,
            "assistant" => {
                let message = &value["message"];
                if message.get("error").is_some() || value.get("error").is_some() {
                    self.fail(status(&value));
                    return Ok(());
                }
                let id = message["id"].as_str().unwrap_or_default();
                if self.native_ids.contains(id) {
                    return Ok(());
                }
                self.native(json!({"type":"message_start","message":message}))
                    .await?;
                for (index, block) in message["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    self.native(
                        json!({"type":"content_block_start","index":index,"content_block":block}),
                    )
                    .await?;
                    if block["type"] == "text"
                        && block["text"].as_str().is_some_and(|text| !text.is_empty())
                    {
                        self.visible = true;
                    }
                    self.native(json!({"type":"content_block_stop","index":index}))
                        .await?;
                }
                self.native(json!({"type":"message_delta","delta":{"stop_reason":message["stop_reason"]},"usage":message["usage"]})).await?;
                self.native(json!({"type":"message_stop"})).await?;
            }
            "result" => {
                if value["is_error"] == true
                    || value["subtype"]
                        .as_str()
                        .is_some_and(|kind| kind.starts_with("error"))
                {
                    self.fail(status(&value));
                } else if self.pending_stop && self.stopped.is_some() {
                    self.finish(false).await?;
                } else {
                    return Err(PluginError::transport(
                        "Claude Code ended a reply without a completed message.",
                    ));
                }
            }
            "system" | "user" | "control_response" | "command_lifecycle" | "rate_limit_event" => {}
            "control_request" => {
                let response = json!({"type":"control_response","response":{"subtype":"error","request_id":value["request_id"],"error":"Only tools explicitly supplied by Nora are permitted."}});
                self.run
                    .input
                    .write_all(format!("{response}\n").as_bytes())
                    .await
                    .map_err(|_| cli::error())?;
            }
            _ => {}
        }
        Ok(())
    }
    async fn next(&mut self) -> Option<Vec<u8>> {
        loop {
            if (self.visible || self.done) && !self.queued.is_empty() {
                return self.queued.pop_front();
            }
            if self.done {
                return None;
            }
            let value = if let Some(value) = self.run.backlog.pop_front() {
                Some(value)
            } else {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(120),
                    self.run.output.recv(),
                )
                .await
                {
                    Ok(value) => value,
                    Err(_) => None,
                }
            };
            match value {
                Some(Ok(value)) => {
                    if let Err(error) = self.outer(value).await {
                        self.fail(if error.code == "authentication_required" {
                            401
                        } else {
                            502
                        });
                    }
                }
                _ => self.fail(502),
            }
        }
    }
}
use tokio::io::AsyncWriteExt;
pub(crate) async fn response(
    run: OwnedMutexGuard<Run>,
    streaming: bool,
    persistence: Arc<dyn Persistence>,
) -> PluginResult<FetchResponse> {
    let mut state = State {
        run,
        persistence,
        queued: VecDeque::new(),
        stream: streaming,
        done: false,
        complete: false,
        visible: false,
        failed: None,
        message: Value::Null,
        content: BTreeMap::new(),
        native_ids: std::collections::BTreeSet::new(),
        stopped: None,
        usage: json!({}),
        pending_stop: false,
    };
    let first = state.next().await;
    if let Some(status) = state.failed.filter(|_| !state.visible || !streaming) {
        return Ok(failure(status));
    }
    let Some(first) = first else {
        return Ok(failure(502));
    };
    let body = stream::once(async move { Ok(first) })
        .chain(stream::unfold(state, |mut state| async move {
            state.next().await.map(|chunk| (Ok(chunk), state))
        }));
    Ok(FetchResponse {
        status: 200,
        headers: BTreeMap::from([(
            "content-type".into(),
            if streaming {
                "text/event-stream".into()
            } else {
                "application/json".into()
            },
        )]),
        body: Box::pin(body),
    })
}
#[cfg(test)]
#[path = "../test/stream.test.rs"]
mod tests;
