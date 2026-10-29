use futures::{stream, StreamExt};
use nora_provider_plugin_sdk::{FetchRequest, FetchResponse, PluginError, PluginResult};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const MAX_BUFFER: usize = 8 * 1024 * 1024;
type Names = BTreeMap<String, (String, String)>;

pub(super) fn flat_name(space: &str, name: &str) -> String {
    let flat = format!("{space}__{name}");
    if flat.len() <= 64 {
        return flat;
    }
    let digest = Sha256::digest(format!("{space}\0{name}").as_bytes());
    let mut end = 55;
    while !flat.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}_{:02x}{:02x}{:02x}{:02x}",
        &flat[..end],
        digest[0],
        digest[1],
        digest[2],
        digest[3]
    )
}

pub(super) fn flatten_call(value: &mut Value) -> bool {
    let Some(object) = value.as_object_mut() else {
        return false;
    };
    let Some(space) = object
        .get("namespace")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return false;
    };
    object.remove("namespace");
    if !space.is_empty() && space != "functions" {
        if let Some(name) = object
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
        {
            object.insert("name".into(), json!(flat_name(&space, name)));
        }
    }
    true
}

fn names(payload: &Value) -> Names {
    let mut names = Names::new();
    let mut add = |space: &str, name: &str| {
        if !space.is_empty() && space != "functions" && !name.is_empty() {
            names.insert(flat_name(space, name), (space.into(), name.into()));
        }
    };
    for tool in payload
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if tool.get("type").and_then(Value::as_str) != Some("namespace") {
            continue;
        }
        let space = tool.get("name").and_then(Value::as_str).unwrap_or_default();
        for tool in tool
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if tool.get("type").and_then(Value::as_str) == Some("function") {
                add(
                    space,
                    tool.get("name").and_then(Value::as_str).unwrap_or_default(),
                );
            }
        }
    }
    for item in payload
        .get("input")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        add(
            item.get("namespace")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            item.get("name").and_then(Value::as_str).unwrap_or_default(),
        );
    }
    names
}

fn restore_call(item: &mut Value, names: &Names) -> bool {
    if item.get("type").and_then(Value::as_str) != Some("function_call")
        || item.get("namespace").is_some_and(|value| !value.is_null())
    {
        return false;
    }
    let Some((space, name)) = item
        .get("name")
        .and_then(Value::as_str)
        .and_then(|name| names.get(name))
    else {
        return false;
    };
    item["name"] = json!(name);
    item["namespace"] = json!(space);
    item["encrypted_function_args"] = json!([]);
    true
}

fn restore(bytes: &[u8], names: &Names) -> Option<Vec<u8>> {
    let mut value: Value = serde_json::from_slice(bytes).ok()?;
    let mut changed = value
        .get_mut("item")
        .is_some_and(|item| restore_call(item, names));
    let response = if value.get("response").is_some_and(Value::is_object) {
        value.get_mut("response")
    } else if value.get("object").and_then(Value::as_str) == Some("response") {
        Some(&mut value)
    } else {
        None
    };
    if let Some(output) = response
        .and_then(|response| response.get_mut("output"))
        .and_then(Value::as_array_mut)
    {
        for item in output {
            changed |= restore_call(item, names);
        }
    }
    changed.then(|| serde_json::to_vec(&value).ok()).flatten()
}

struct Tidy {
    names: Names,
    sse: bool,
    buffer: Vec<u8>,
    passthrough: bool,
}
impl Tidy {
    fn line(&self, bytes: &[u8]) -> Vec<u8> {
        let end = bytes.len()
            - if bytes.ends_with(b"\r\n") {
                2
            } else if bytes.ends_with(b"\n") {
                1
            } else {
                0
            };
        let Some(data) = bytes[..end].strip_prefix(b"data:") else {
            return bytes.to_vec();
        };
        let Some(restored) = restore(data, &self.names) else {
            return bytes.to_vec();
        };
        let mut result = b"data: ".to_vec();
        result.extend(restored);
        result.extend_from_slice(&bytes[end..]);
        result
    }
    fn write(&mut self, bytes: &[u8]) -> Vec<u8> {
        if !self.sse {
            if self.passthrough {
                return bytes.to_vec();
            }
            self.buffer.extend_from_slice(bytes);
            if self.buffer.len() > MAX_BUFFER {
                self.passthrough = true;
                return std::mem::take(&mut self.buffer);
            }
            return Vec::new();
        }
        let mut result = Vec::new();
        for part in bytes.split_inclusive(|byte| *byte == b'\n') {
            if self.passthrough {
                result.extend_from_slice(part);
                if part.ends_with(b"\n") {
                    self.passthrough = false;
                }
                continue;
            }
            self.buffer.extend_from_slice(part);
            if self.buffer.len() > MAX_BUFFER {
                self.passthrough = !self.buffer.ends_with(b"\n");
                result.extend(std::mem::take(&mut self.buffer));
            } else if self.buffer.ends_with(b"\n") {
                let line = std::mem::take(&mut self.buffer);
                result.extend(self.line(&line));
            }
        }
        result
    }
    fn flush(&mut self) -> Vec<u8> {
        let bytes = std::mem::take(&mut self.buffer);
        if self.sse {
            self.line(&bytes)
        } else {
            restore(&bytes, &self.names).unwrap_or(bytes)
        }
    }
}

pub(super) fn response(
    request: &FetchRequest,
    mut response: FetchResponse,
) -> PluginResult<FetchResponse> {
    if response.status >= 400
        || !super::is_oauth(request.credentials.as_ref())
        || !url::Url::parse(&request.url)
            .ok()
            .is_some_and(|url| url.path().ends_with("/responses"))
    {
        return Ok(response);
    }
    let payload: Value = match request
        .decode_body()?
        .and_then(|body| serde_json::from_slice(&body).ok())
    {
        Some(payload) => payload,
        None => return Ok(response),
    };
    let names = names(&payload);
    if names.is_empty() {
        return Ok(response);
    }
    let kind = response
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
        });
    let sse = match kind.as_deref() {
        Some("text/event-stream") => true,
        Some(kind) if kind == "application/json" || kind.ends_with("+json") => false,
        None => payload
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        _ => return Ok(response),
    };
    response
        .headers
        .retain(|name, _| !name.eq_ignore_ascii_case("content-length"));
    let tidy = Tidy {
        names,
        sse,
        buffer: Vec::new(),
        passthrough: false,
    };
    let state = (response.body, tidy, None::<PluginError>, false);
    response.body = Box::pin(stream::unfold(
        state,
        |(mut body, mut tidy, mut pending, mut done)| async move {
            if let Some(error) = pending.take() {
                return Some((Err(error), (body, tidy, pending, true)));
            }
            if done {
                return None;
            }
            loop {
                let bytes = match body.next().await {
                    Some(Ok(bytes)) => tidy.write(&bytes),
                    Some(Err(error)) => {
                        pending = Some(error);
                        done = true;
                        tidy.flush()
                    }
                    None => {
                        done = true;
                        tidy.flush()
                    }
                };
                if !bytes.is_empty() {
                    return Some((Ok(bytes), (body, tidy, pending, done)));
                }
                if let Some(error) = pending.take() {
                    return Some((Err(error), (body, tidy, pending, true)));
                }
                if done {
                    return None;
                }
            }
        },
    ));
    Ok(response)
}

#[cfg(test)]
#[path = "../test/namespaces.test.rs"]
mod tests;
