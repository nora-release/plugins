use super::{
    network,
    protobuf::{self, fields},
    request,
};
use futures::StreamExt;
use nora_ai::{HttpByteStream, HttpStreamingResponse};
use nora_provider_plugin_sdk::{FetchResponse, PluginError, PluginResult};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::Duration,
};
fn truncated() -> PluginError {
    PluginError::transport("The Devin reply was malformed or cut short.")
}
struct Reader {
    body: HttpByteStream,
    buffer: Vec<u8>,
    ended: bool,
    total: usize,
    deadline: tokio::time::Instant,
}
impl Reader {
    async fn next(&mut self) -> PluginResult<Option<(bool, Vec<u8>)>> {
        if self.ended {
            return Ok(None);
        }
        loop {
            if self.buffer.len() >= 5 {
                let flags = self.buffer[0];
                if flags > 3 {
                    return Err(truncated());
                }
                let length = u32::from_be_bytes(self.buffer[1..5].try_into().unwrap()) as usize;
                if length > protobuf::MAX_FRAME {
                    return Err(truncated());
                }
                if self.buffer.len() >= length + 5 {
                    let bytes = self.buffer.drain(..length + 5).collect::<Vec<_>>();
                    let bytes = if flags & 1 != 0 {
                        protobuf::inflate(&bytes[5..])?
                    } else {
                        bytes[5..].to_vec()
                    };
                    if flags & 2 != 0 {
                        self.ended = true;
                        if !self.buffer.is_empty() {
                            return Err(truncated());
                        }
                    }
                    return Ok(Some((flags & 2 != 0, bytes)));
                }
            }
            let item = tokio::time::timeout_at(
                self.deadline
                    .min(tokio::time::Instant::now() + Duration::from_secs(120)),
                self.body.next(),
            )
            .await
            .map_err(|_| PluginError::transport("The Devin reply timed out."))?;
            match item {
                Some(Ok(bytes)) => {
                    self.total = self.total.saturating_add(bytes.len());
                    if self.total > 128 * 1024 * 1024
                        || self.buffer.len().saturating_add(bytes.len())
                            > protobuf::MAX_FRAME + 65536
                    {
                        return Err(truncated());
                    }
                    self.buffer.extend_from_slice(&bytes);
                }
                Some(Err(_)) | None => return Err(truncated()),
            }
        }
    }
}
struct Tool {
    native_id: String,
    id: String,
    name: String,
    args: String,
    opened: bool,
}
#[derive(Default)]
struct Usage {
    input: u64,
    output: u64,
    read: u64,
    write: u64,
}
struct Decoder {
    reader: Reader,
    events: VecDeque<Value>,
    tools: Vec<Tool>,
    last: Option<usize>,
    stop: u64,
    said: u64,
    usage: Usage,
    finished: bool,
    zero_args: BTreeSet<String>,
}
impl Decoder {
    fn tool(&mut self, data: &[u8]) -> PluginResult<()> {
        let (mut native_id, mut name, mut args) = (String::new(), String::new(), String::new());
        for field in fields(data)? {
            match (field.key, field.wire) {
                (1, 2) => native_id = field.text()?.into(),
                (2, 2) => name = field.text()?.into(),
                (3, 2) => args.push_str(field.text()?),
                _ => {}
            }
        }
        if native_id.len() > 8192 || name.len() > 512 || args.len() > 8 * 1024 * 1024 {
            return Err(truncated());
        }
        let existing = if !native_id.is_empty() {
            self.tools.iter().position(|t| t.native_id == native_id)
        } else if name.is_empty() {
            self.last
        } else {
            None
        };
        let index = if let Some(index) = existing {
            index
        } else {
            if self.tools.len() >= 128 || native_id.is_empty() && name.is_empty() {
                return Err(truncated());
            }
            let id = if native_id.is_empty() {
                format!("call_{}", uuid::Uuid::new_v4().simple())
            } else {
                request::out_id(&native_id)
            };
            self.tools.push(Tool {
                native_id,
                id,
                name: String::new(),
                args: String::new(),
                opened: false,
            });
            self.tools.len() - 1
        };
        self.last = Some(index);
        let tool = &mut self.tools[index];
        if !name.is_empty() {
            if !tool.name.is_empty() && tool.name != name {
                return Err(truncated());
            }
            tool.name = name;
        }
        if tool.args.len() + args.len() > 8 * 1024 * 1024 {
            return Err(truncated());
        }
        if !tool.opened && !tool.name.is_empty() {
            tool.opened = true;
            self.events.push_back(json!({"tool_calls":[{"index":index,"id":tool.id,"type":"function","function":{"name":tool.name,"arguments":""}}]}));
            if !tool.args.is_empty() {
                self.events.push_back(
                    json!({"tool_calls":[{"index":index,"function":{"arguments":tool.args}}]}),
                );
            }
        }
        tool.args.push_str(&args);
        if tool.opened && !args.is_empty() {
            self.events
                .push_back(json!({"tool_calls":[{"index":index,"function":{"arguments":args}}]}));
        }
        Ok(())
    }
    fn message(&mut self, data: &[u8]) -> PluginResult<()> {
        for field in fields(data)? {
            match (field.key, field.wire) {
                (3, 2) | (9, 2) => {
                    let text = field.text()?;
                    if !text.is_empty() {
                        self.said = self.said.saturating_add(text.len() as u64);
                        self.events.push_back(if field.key == 3 {
                            json!({"content":text})
                        } else {
                            json!({"reasoning_content":text})
                        });
                    }
                }
                (6, 2) => self.tool(field.data)?,
                (5, 0) => self.stop = field.number,
                (7, 2) => {
                    for f in fields(field.data)? {
                        if f.wire != 0 {
                            continue;
                        }
                        match f.key {
                            2 => self.usage.input = f.number,
                            3 => self.usage.output = f.number,
                            4 => self.usage.write = f.number,
                            5 => self.usage.read = f.number,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    fn finish(&mut self, data: &[u8]) -> PluginResult<()> {
        let value: Value = serde_json::from_slice(data).map_err(|_| truncated())?;
        if !value.is_object() {
            return Err(truncated());
        }
        let status = network::status(200, &value);
        if status >= 300 {
            return Err(network::error(status));
        }
        if value.get("error").is_some_and(|v| !v.is_null()) {
            return Err(truncated());
        }
        for (index, tool) in self.tools.iter_mut().enumerate() {
            if !tool.opened || tool.name.is_empty() {
                return Err(truncated());
            }
            if tool.args.trim().is_empty() {
                if !self.zero_args.contains(&tool.name) {
                    return Err(truncated());
                }
                tool.args = "{}".into();
                self.events.push_back(
                    json!({"tool_calls":[{"index":index,"function":{"arguments":"{}"}}]}),
                );
            }
            let value: Value = serde_json::from_str(&tool.args).map_err(|_| truncated())?;
            if !value.is_object() {
                return Err(truncated());
            }
        }
        if self.stop == 10 && self.tools.is_empty() {
            return Err(truncated());
        }
        self.finished = true;
        Ok(())
    }
    fn usage(&self) -> Value {
        let prompt = self
            .usage
            .input
            .saturating_add(self.usage.read)
            .saturating_add(self.usage.write);
        let estimated = self.usage.output == 0 && self.said > 0;
        let completion = if estimated {
            self.said.saturating_add(3) / 4
        } else {
            self.usage.output
        };
        json!({"prompt_tokens":prompt,"completion_tokens":completion,"total_tokens":prompt.saturating_add(completion),"prompt_tokens_details":{"cached_tokens":self.usage.read,"cache_write_tokens":self.usage.write},"estimated":estimated})
    }
    fn stop(&self) -> &'static str {
        if !self.tools.is_empty() || self.stop == 10 {
            "tool_calls"
        } else if self.stop == 3 {
            "length"
        } else {
            "stop"
        }
    }
    async fn next(&mut self) -> PluginResult<Option<Value>> {
        loop {
            if let Some(event) = self.events.pop_front() {
                return Ok(Some(event));
            }
            if self.finished {
                return Ok(None);
            }
            let Some((end, data)) = self.reader.next().await? else {
                return Err(truncated());
            };
            if end {
                self.finish(&data)?;
                continue;
            }
            self.message(&data)?;
        }
    }
}
pub(crate) fn failure(status: u16) -> FetchResponse {
    FetchResponse::bytes(status,BTreeMap::from([("content-type".into(),"application/json".into())]),json!({"error":{"message":network::error(status).message,"type":"devin_error","code":status}}).to_string().into_bytes())
}
fn code(error: &PluginError) -> u16 {
    if error.code == "authentication_required" {
        401
    } else {
        error
            .code
            .strip_prefix("devin_http_")
            .and_then(|v| v.parse().ok())
            .unwrap_or(502)
    }
}
fn chunk(
    id: &str,
    model: &str,
    delta: Value,
    finish: Option<&str>,
    usage: Option<Value>,
) -> Vec<u8> {
    let mut value = json!({"id":id,"object":"chat.completion.chunk","created":nora_provider_subscription_core::now()/1000,"model":model,"choices":[{"index":0,"delta":delta,"finish_reason":finish}]});
    if let Some(usage) = usage {
        value["usage"] = usage;
    }
    format!("data: {value}\n\n").into_bytes()
}
struct Stream {
    decoder: Decoder,
    first: Option<Value>,
    id: String,
    model: String,
    stage: u8,
}
impl Stream {
    async fn next(&mut self) -> PluginResult<Option<Vec<u8>>> {
        if self.stage == 0 {
            self.stage = 1;
            return Ok(Some(chunk(
                &self.id,
                &self.model,
                json!({"role":"assistant","content":""}),
                None,
                None,
            )));
        }
        if self.stage == 2 {
            self.stage = 3;
            return Ok(Some(b"data: [DONE]\n\n".to_vec()));
        }
        if self.stage == 3 {
            return Ok(None);
        }
        let next = if let Some(value) = self.first.take() {
            Some(value)
        } else {
            self.decoder.next().await?
        };
        if let Some(delta) = next {
            return Ok(Some(chunk(&self.id, &self.model, delta, None, None)));
        }
        self.stage = 2;
        Ok(Some(chunk(
            &self.id,
            &self.model,
            json!({}),
            Some(self.decoder.stop()),
            Some(self.decoder.usage()),
        )))
    }
}
pub(crate) async fn response(
    response: HttpStreamingResponse,
    chat: &Value,
) -> PluginResult<FetchResponse> {
    if !(200..300).contains(&response.status) {
        let status = response.status;
        let bytes = network::collect(response, 1024 * 1024).await?;
        return Ok(failure(network::status(
            status,
            &serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )));
    }
    let mut decoder = Decoder {
        reader: Reader {
            body: response.body,
            buffer: Vec::new(),
            ended: false,
            total: 0,
            deadline: tokio::time::Instant::now() + Duration::from_secs(900),
        },
        events: VecDeque::new(),
        tools: Vec::new(),
        last: None,
        stop: 0,
        said: 0,
        usage: Usage::default(),
        finished: false,
        zero_args: chat["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|tool| {
                let f = &tool["function"];
                let schema = &f["parameters"];
                if schema["type"] == "object"
                    && schema["required"].as_array().is_none_or(|v| v.is_empty())
                    && !["allOf", "anyOf", "oneOf"]
                        .iter()
                        .any(|k| schema.get(*k).is_some())
                {
                    f["name"].as_str().map(str::to_owned)
                } else {
                    None
                }
            })
            .collect(),
    };
    let first = match decoder.next().await {
        Ok(Some(event)) => event,
        Ok(None) => return Ok(failure(502)),
        Err(error) => return Ok(failure(code(&error))),
    };
    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
    let model = chat["model"].as_str().unwrap_or_default().to_owned();
    if chat["stream"] == true {
        let stream = Stream {
            decoder,
            first: Some(first),
            id,
            model,
            stage: 0,
        };
        return Ok(FetchResponse {
            status: 200,
            headers: BTreeMap::from([
                ("content-type".into(), "text/event-stream".into()),
                ("cache-control".into(), "no-cache".into()),
            ]),
            body: Box::pin(futures::stream::try_unfold(
                stream,
                |mut stream| async move {
                    stream
                        .next()
                        .await
                        .map(|next| next.map(|bytes| (bytes, stream)))
                },
            )),
        });
    }
    let mut message = json!({"role":"assistant","content":""});
    let mut text = String::new();
    let mut thought = String::new();
    let mut event = Some(first);
    loop {
        if let Some(value) = event.take() {
            text.push_str(value["content"].as_str().unwrap_or_default());
            thought.push_str(value["reasoning_content"].as_str().unwrap_or_default());
        }
        event = match decoder.next().await {
            Ok(Some(event)) => Some(event),
            Ok(None) => break,
            Err(error) => return Ok(failure(code(&error))),
        };
    }
    message["content"] = json!(text);
    if !thought.is_empty() {
        message["reasoning_content"] = json!(thought);
    }
    if !decoder.tools.is_empty() {
        message["tool_calls"]=json!(decoder.tools.iter().map(|tool|json!({"id":tool.id,"type":"function","function":{"name":tool.name,"arguments":tool.args}})).collect::<Vec<_>>());
    }
    Ok(FetchResponse::bytes(200,BTreeMap::from([("content-type".into(),"application/json".into())]),json!({"id":id,"object":"chat.completion","created":nora_provider_subscription_core::now()/1000,"model":model,"choices":[{"index":0,"message":message,"finish_reason":decoder.stop()}],"usage":decoder.usage()}).to_string().into_bytes()))
}
#[cfg(test)]
#[path = "../test/stream.test.rs"]
mod tests;
