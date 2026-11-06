use super::{models::Wire, network, schema};
use futures::StreamExt;
use nora_ai::{HttpByteStream, HttpStreamingResponse};
use nora_provider_plugin_sdk::{FetchResponse, PluginError, PluginResult};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::Duration,
};
fn headers(mut headers: BTreeMap<String, String>) -> BTreeMap<String, String> {
    headers.retain(|k, _| {
        !matches!(
            k.to_ascii_lowercase().as_str(),
            "content-length"
                | "content-encoding"
                | "transfer-encoding"
                | "connection"
                | "set-cookie"
                | "proxy-authenticate"
                | "proxy-authorization"
        )
    });
    headers
}
pub(crate) fn failure(wire: Wire, status: u16) -> FetchResponse {
    failure_message(wire, status, network::error(status).message)
}

pub(crate) fn too_large(wire: Wire, omitted: usize) -> FetchResponse {
    let mut message = network::error(413).message;
    if omitted > 0 {
        message.push_str(&format!(
            " The request was still too large with {omitted} older images or files left out."
        ));
    }
    failure_message(wire, 413, message)
}

fn failure_message(wire: Wire, status: u16, message: String) -> FetchResponse {
    let error = match wire {
        Wire::Anthropic => {
            json!({"type":"error","error":{"type":match status{401=>"authentication_error",403=>"permission_error",413=>"request_too_large",429=>"rate_limit_error",_=>"api_error"},"message":message}})
        }
        Wire::Gemini => json!({"error":{"code":status,"message":message,"status":"INTERNAL"}}),
        _ => {
            json!({"error":{"message":message,"code":status,"type":if status == 413 {"invalid_request_error"} else {"api_error"}}})
        }
    };
    FetchResponse::bytes(
        status,
        BTreeMap::from([("content-type".into(), "application/json".into())]),
        error.to_string().into_bytes(),
    )
}
struct Block {
    initial: Value,
    json: String,
}
pub(crate) struct Tools {
    names: BTreeSet<String>,
    blocks: BTreeMap<u64, Block>,
}
impl Tools {
    pub fn new(names: BTreeSet<String>) -> Self {
        Self {
            names,
            blocks: BTreeMap::new(),
        }
    }
    pub fn finish(&self) -> PluginResult<()> {
        if self.blocks.is_empty() {
            Ok(())
        } else {
            Err(schema::malformed())
        }
    }
    pub fn frame(&mut self, raw: &str) -> PluginResult<Vec<Vec<u8>>> {
        let raw = raw.trim_end_matches(['\r', '\n']);
        let lines = raw.lines().collect::<Vec<_>>();
        let data = lines
            .iter()
            .filter_map(|line| {
                line.strip_prefix("data:")
                    .map(|s| s.strip_prefix(' ').unwrap_or(s))
            })
            .collect::<Vec<_>>()
            .join("\n");
        let unchanged = || format!("{raw}\n\n").into_bytes();
        if data.is_empty() {
            return Ok(vec![unchanged()]);
        }
        if data == "[DONE]" {
            self.finish()?;
            return Ok(vec![unchanged()]);
        }
        let mut event: Value = serde_json::from_str(&data)
            .map_err(|_| PluginError::transport("Factory returned an invalid stream event."))?;
        let replace = |value: &Value| {
            let mut output = Vec::new();
            let mut first = true;
            for line in &lines {
                if line.starts_with("data:") {
                    if first {
                        output.push(format!("data: {value}"));
                        first = false;
                    }
                } else {
                    output.push((*line).into());
                }
            }
            format!("{}\n\n", output.join("\n")).into_bytes()
        };
        let kind = event["type"].as_str().unwrap_or_default().to_owned();
        let index = event["index"].as_u64();
        if kind == "content_block_start"
            && event["content_block"]["type"] == "tool_use"
            && event["content_block"]["name"]
                .as_str()
                .is_some_and(|s| self.names.contains(s))
        {
            let index = index.ok_or_else(schema::malformed)?;
            if self.blocks.len() >= 128 || self.blocks.contains_key(&index) {
                return Err(schema::malformed());
            }
            self.blocks.insert(
                index,
                Block {
                    initial: event["content_block"]["input"].take(),
                    json: String::new(),
                },
            );
            event["content_block"]["input"] = json!({});
            return Ok(vec![replace(&event)]);
        }
        if let Some(index) = index {
            if kind == "content_block_delta" && event["delta"]["type"] == "input_json_delta" {
                if let Some(block) = self.blocks.get_mut(&index) {
                    let text = event["delta"]["partial_json"]
                        .as_str()
                        .ok_or_else(schema::malformed)?;
                    if block.json.len() + text.len() > 8 * 1024 * 1024 {
                        return Err(schema::malformed());
                    }
                    block.json.push_str(text);
                    return Ok(Vec::new());
                }
            }
            if kind == "content_block_stop" {
                if let Some(block) = self.blocks.remove(&index) {
                    let value = if block.json.is_empty() {
                        block.initial
                    } else {
                        serde_json::from_str(&block.json).map_err(|_| schema::malformed())?
                    };
                    let inner = schema::unwrap(value)?;
                    let delta = json!({"type":"content_block_delta","index":index,"delta":{"type":"input_json_delta","partial_json":inner.to_string()}});
                    return Ok(vec![
                        format!("event: content_block_delta\ndata: {delta}\n\n").into_bytes(),
                        unchanged(),
                    ]);
                }
            }
        }
        if kind == "error" {
            self.blocks.clear();
        }
        if kind == "message_stop" {
            self.finish()?;
        }
        Ok(vec![unchanged()])
    }
}
struct Reader {
    body: HttpByteStream,
    buffer: Vec<u8>,
    ready: VecDeque<Vec<u8>>,
    tools: Tools,
    eof: bool,
    total: usize,
    deadline: tokio::time::Instant,
}
fn boundary(bytes: &[u8]) -> Option<(usize, usize)> {
    for i in 0..bytes.len() {
        for end in [
            b"\r\n\r\n".as_slice(),
            b"\r\n\n",
            b"\n\r\n",
            b"\n\n",
            b"\r\r",
        ] {
            if bytes[i..].starts_with(end) {
                return Some((i, end.len()));
            }
        }
    }
    None
}
impl Reader {
    async fn next(&mut self) -> PluginResult<Option<Vec<u8>>> {
        loop {
            if let Some(bytes) = self.ready.pop_front() {
                return Ok(Some(bytes));
            }
            if let Some((index, ending)) = boundary(&self.buffer) {
                let bytes = self.buffer.drain(..index + ending).collect::<Vec<_>>();
                let text = std::str::from_utf8(&bytes)
                    .map_err(|_| PluginError::transport("Factory returned invalid UTF-8."))?;
                self.ready.extend(self.tools.frame(text)?);
                continue;
            }
            if self.eof {
                if !self.buffer.is_empty() {
                    let bytes = std::mem::take(&mut self.buffer);
                    let text = std::str::from_utf8(&bytes).map_err(|_| schema::malformed())?;
                    self.ready.extend(self.tools.frame(text)?);
                    continue;
                }
                self.tools.finish()?;
                return Ok(None);
            }
            match tokio::time::timeout_at(
                self.deadline
                    .min(tokio::time::Instant::now() + Duration::from_secs(120)),
                self.body.next(),
            )
            .await
            .map_err(|_| PluginError::transport("The Factory stream timed out."))?
            {
                Some(Ok(chunk)) => {
                    self.total += chunk.len();
                    if self.buffer.len() + chunk.len() > 8 * 1024 * 1024
                        || self.total > 128 * 1024 * 1024
                    {
                        return Err(PluginError::transport(
                            "The Factory stream exceeded its size limit.",
                        ));
                    }
                    self.buffer.extend_from_slice(&chunk);
                }
                Some(Err(_)) => {
                    return Err(PluginError::transport("The Factory stream broke off."))
                }
                None => self.eof = true,
            }
        }
    }
}
pub(crate) async fn response(
    response: HttpStreamingResponse,
    names: BTreeSet<String>,
) -> PluginResult<FetchResponse> {
    let status = response.status;
    let headers = headers(response.headers.clone());
    if !names.is_empty() {
        if headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v.contains("text/event-stream"))
        {
            let reader = Reader {
                body: response.body,
                buffer: Vec::new(),
                ready: VecDeque::new(),
                tools: Tools::new(names),
                eof: false,
                total: 0,
                deadline: tokio::time::Instant::now() + Duration::from_secs(900),
            };
            return Ok(FetchResponse {
                status,
                headers,
                body: Box::pin(futures::stream::try_unfold(
                    reader,
                    |mut reader| async move {
                        reader
                            .next()
                            .await
                            .map(|value| value.map(|bytes| (bytes, reader)))
                    },
                )),
            });
        }
        let bytes = nora_provider_subscription_core::collect(response, 16 * 1024 * 1024).await?;
        let mut value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| PluginError::transport("Factory returned invalid JSON."))?;
        schema::response(&mut value, &names)?;
        return Ok(FetchResponse::bytes(
            status,
            headers,
            value.to_string().into_bytes(),
        ));
    }
    let body = futures::stream::try_unfold(
        (
            response.body,
            tokio::time::Instant::now() + Duration::from_secs(900),
            0usize,
        ),
        |(mut stream, deadline, total)| async move {
            let item = tokio::time::timeout_at(
                deadline.min(tokio::time::Instant::now() + Duration::from_secs(120)),
                stream.next(),
            )
            .await
            .map_err(|_| PluginError::transport("The Factory reply timed out."))?;
            match item {
                None => Ok(None),
                Some(Err(_)) => Err(PluginError::transport("The Factory reply broke off.")),
                Some(Ok(bytes)) => {
                    let total = total + bytes.len();
                    if total > 128 * 1024 * 1024 {
                        return Err(PluginError::transport(
                            "The Factory reply exceeded its size limit.",
                        ));
                    }
                    Ok(Some((bytes.to_vec(), (stream, deadline, total))))
                }
            }
        },
    );
    Ok(FetchResponse {
        status,
        headers,
        body: Box::pin(body),
    })
}
#[cfg(test)]
#[path = "../test/stream.test.rs"]
mod tests;
