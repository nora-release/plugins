use futures::StreamExt;
use nora_provider_plugin_sdk::{FetchResponse, PluginError, PluginResult, ResponseBody};
use nora_provider_subscription_core::MAX_JSON;
use serde_json::Value;

const SEARCH: &str = "ToolSearch";
const ALIAS: &str = "ToolSearchTool";
const PLACEHOLDER: &str = "DeferredToolPlaceholder";

fn contains(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|part| part == needle)
}

// The literal ToolSearch name makes ZCode hide ordinary tools. Rename only
// when it is declared and the caller has not reserved the alias itself.
pub(crate) fn rename_request(body: &mut Value, original: &[u8]) -> bool {
    if !contains(original, br#""ToolSearch""#) {
        return false;
    }
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else {
        return false;
    };
    if !tools.iter().any(|tool| tool["name"] == SEARCH)
        || tools.iter().any(|tool| tool["name"] == ALIAS)
    {
        return false;
    }
    tools.retain(|tool| tool["name"] != PLACEHOLDER);
    for tool in tools {
        // JavaScript object spread also preserves an array's indexed values.
        if let Value::Array(items) = tool {
            *tool = Value::Object(
                std::mem::take(items)
                    .into_iter()
                    .enumerate()
                    .map(|(index, value)| (index.to_string(), value))
                    .collect(),
            );
        }
        if let Some(tool) = tool.as_object_mut() {
            tool.remove("defer_loading");
            if tool.get("name").is_some_and(|name| name == SEARCH) {
                tool.insert("name".into(), Value::String(ALIAS.into()));
            }
        }
    }
    if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages {
            if message["role"] != "assistant" {
                continue;
            }
            if let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) {
                for block in content {
                    rename_call(block, SEARCH, ALIAS);
                }
            }
        }
    }
    if body["tool_choice"]["type"] == "tool" && body["tool_choice"]["name"] == SEARCH {
        body["tool_choice"]["name"] = Value::String(ALIAS.into());
    }
    true
}

fn rename_call(block: &mut Value, from: &str, to: &str) -> bool {
    if block["type"] != "tool_use" || block["name"] != from {
        return false;
    }
    block["name"] = Value::String(to.into());
    true
}

fn json_body(bytes: Vec<u8>) -> Vec<u8> {
    if !contains(&bytes, br#""ToolSearchTool""#) {
        return bytes;
    }
    let Ok(mut body) = serde_json::from_slice::<Value>(&bytes) else {
        return bytes;
    };
    let Some(content) = body.get_mut("content").and_then(Value::as_array_mut) else {
        return bytes;
    };
    let mut changed = false;
    for block in content {
        changed |= rename_call(block, ALIAS, SEARCH);
    }
    if changed {
        serde_json::to_vec(&body).unwrap()
    } else {
        bytes
    }
}

fn sse_line(line: &[u8], output: &mut Vec<u8>) {
    let renamed = line
        .strip_prefix(b"data:")
        .filter(|_| contains(line, br#""ToolSearchTool""#))
        .and_then(|data| serde_json::from_slice::<Value>(data).ok())
        .and_then(|mut body| {
            rename_call(body.get_mut("content_block")?, ALIAS, SEARCH).then_some(body)
        });
    if let Some(body) = renamed {
        output.extend_from_slice(b"data: ");
        serde_json::to_writer(output, &body).unwrap();
    } else {
        output.extend_from_slice(line);
    }
}

#[derive(Default)]
struct Sse {
    pending: Vec<u8>,
}

impl Sse {
    fn push(&mut self, bytes: &[u8]) -> PluginResult<Vec<u8>> {
        let mut output = Vec::new();
        for part in bytes.split_inclusive(|byte| *byte == b'\n') {
            if part.len() > MAX_JSON.saturating_sub(self.pending.len()) {
                return Err(PluginError::transport(
                    "ZCode's response line exceeds its size limit.",
                ));
            }
            self.pending.extend_from_slice(part);
            if part.last() == Some(&b'\n') {
                sse_line(&self.pending[..self.pending.len() - 1], &mut output);
                output.push(b'\n');
                self.pending.clear();
            }
        }
        Ok(output)
    }

    fn finish(self) -> Vec<u8> {
        let mut output = Vec::new();
        sse_line(&self.pending, &mut output);
        output
    }
}

fn sse_body(body: ResponseBody) -> ResponseBody {
    Box::pin(futures::stream::unfold(
        (Some(body), Sse::default()),
        |(body, mut decoder)| async move {
            let mut body = body?;
            loop {
                match body.next().await {
                    Some(Ok(bytes)) => match decoder.push(&bytes) {
                        Ok(output) if output.is_empty() => continue,
                        Ok(output) => return Some((Ok(output), (Some(body), decoder))),
                        Err(error) => return Some((Err(error), (None, Sse::default()))),
                    },
                    Some(Err(error)) => return Some((Err(error), (None, Sse::default()))),
                    None => {
                        let output = decoder.finish();
                        return (!output.is_empty())
                            .then_some((Ok(output), (None, Sse::default())));
                    }
                }
            }
        },
    ))
}

pub(crate) fn restore_response(mut response: FetchResponse) -> FetchResponse {
    let content_type = response
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| value.as_str())
        .unwrap_or("");
    if content_type.contains("text/event-stream") {
        response.body = sse_body(response.body);
    } else if content_type.contains("json") {
        let mut body = response.body;
        response.body = Box::pin(futures::stream::once(async move {
            let mut bytes = Vec::new();
            while let Some(chunk) = body.next().await {
                let chunk = chunk?;
                if chunk.len() > MAX_JSON.saturating_sub(bytes.len()) {
                    return Err(PluginError::transport(
                        "ZCode's response exceeds its size limit.",
                    ));
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(json_body(bytes))
        }));
    }
    response
}
