use super::{account, network, protobuf::Pb, schema};
use base64::Engine;
use nora_provider_plugin_sdk::{FetchRequest, PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
pub(crate) fn out_id(id: &str) -> String {
    if !id.is_empty()
        && !id.starts_with("dv_")
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        id.into()
    } else {
        format!(
            "dv_{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(id)
        )
    }
}
pub(crate) fn in_id(id: &str) -> String {
    if let Some(value) = id.strip_prefix("dv_") {
        if let Ok(raw) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(value) {
            if let Ok(raw) = String::from_utf8(raw) {
                if !raw.is_empty() && out_id(&raw) == id {
                    return raw;
                }
            }
        }
    }
    id.into()
}
fn unsupported() -> PluginError {
    PluginError::new("unsupported_media","Devin accepts text and inline images in this native protocol; this message contains unsupported content.")
}
#[derive(Clone, Debug)]
pub(crate) struct Image {
    pub data: String,
    pub mime: String,
}
fn image(part: &Value) -> PluginResult<Image> {
    let value = part["image_url"]
        .as_str()
        .or_else(|| part["image_url"]["url"].as_str())
        .ok_or_else(unsupported)?;
    let (mime, data) = value
        .strip_prefix("data:")
        .and_then(|v| v.split_once(","))
        .ok_or_else(unsupported)?;
    let mime = mime.strip_suffix(";base64").ok_or_else(unsupported)?;
    if !["image/png", "image/jpeg", "image/gif", "image/webp"].contains(&mime)
        || data.len() > 28 * 1024 * 1024
    {
        return Err(unsupported());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| unsupported())?;
    if bytes.is_empty() || bytes.len() > 20 * 1024 * 1024 {
        return Err(unsupported());
    }
    Ok(Image {
        data: data.into(),
        mime: mime.into(),
    })
}
fn content(value: &Value) -> PluginResult<(String, Vec<Image>)> {
    if value.is_null() {
        return Ok((String::new(), Vec::new()));
    }
    if let Some(text) = value.as_str() {
        return Ok((text.into(), Vec::new()));
    }
    let parts = value.as_array().ok_or_else(unsupported)?;
    let mut text: Vec<String> = Vec::new();
    let mut images = Vec::new();
    for part in parts {
        match part["type"].as_str() {
            Some("text") => text.push(part["text"].as_str().ok_or_else(unsupported)?.to_owned()),
            Some("image_url") => images.push(image(part)?),
            _ => return Err(unsupported()),
        }
    }
    Ok((text.join("\n\n"), images))
}
#[derive(Clone, Debug)]
pub(crate) struct Call {
    pub id: String,
    pub name: String,
    pub args: String,
}
#[derive(Clone, Debug)]
pub(crate) struct Message {
    pub role: u64,
    pub text: String,
    pub calls: Vec<Call>,
    pub answer: String,
    pub images: Vec<Image>,
}
impl Message {
    fn new(role: u64, text: String) -> Self {
        Self {
            role,
            text,
            calls: Vec::new(),
            answer: String::new(),
            images: Vec::new(),
        }
    }
    fn encode(&self) -> Pb {
        let mut msg = Pb::new()
            .text(1, &uuid::Uuid::new_v4().to_string())
            .int(2, self.role);
        if !self.text.is_empty() {
            msg = msg.text(3, &self.text);
        }
        for call in &self.calls {
            msg = msg.bytes(
                6,
                Pb::new()
                    .text(1, &call.id)
                    .text(2, &call.name)
                    .text(3, &call.args),
            );
        }
        if !self.answer.is_empty() {
            msg = msg.text(7, &self.answer);
        }
        for image in &self.images {
            msg = msg.bytes(10, Pb::new().text(1, &image.data).text(2, &image.mime));
        }
        msg
    }
}
fn missing(messages: &mut Vec<Message>, pending: &mut Vec<Call>) {
    for call in pending.drain(..) {
        let mut message = Message::new(
            4,
            "Tool use was interrupted and did not produce a result.".into(),
        );
        message.answer = call.id;
        messages.push(message);
    }
}
fn format(value: &Value) -> String {
    match value["type"].as_str() {
        Some("json_object") => "Respond with a single JSON object and nothing else.".into(),
        Some("json_schema") => {
            let schema = value
                .pointer("/json_schema/schema")
                .or_else(|| value.get("schema"));
            if let Some(schema) = schema.filter(|v| !v.is_null()) {
                format!("Respond with a single JSON value and nothing else, matching this JSON schema:\n{schema}.")
            } else {
                "Respond with a single JSON object and nothing else.".into()
            }
        }
        _ => String::new(),
    }
}
pub(crate) fn parse(request: &FetchRequest) -> PluginResult<Value> {
    let url = url::Url::parse(&request.url)
        .map_err(|_| PluginError::invalid("The Devin request URL is invalid."))?;
    if request.method != "POST"
        || url.origin() != url::Url::parse(network::SERVER).unwrap().origin()
        || url.path() != "/devin/v1/chat/completions"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(PluginError::invalid(
            "Devin only serves its declared chat completions endpoint.",
        ));
    }
    if request
        .body_file
        .as_ref()
        .is_some_and(|f| f.length > 64 * 1024 * 1024)
    {
        return Err(PluginError::invalid("The Devin request exceeds 64 MiB."));
    }
    let bytes = request
        .decode_body()?
        .ok_or_else(|| PluginError::invalid("The Devin request body is missing."))?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err(PluginError::invalid("The Devin request exceeds 64 MiB."));
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| PluginError::invalid("The Devin request is invalid JSON."))?;
    if !value.is_object()
        || account::text(value.get("model"), 512).is_none()
        || !value["messages"].is_array()
    {
        return Err(PluginError::invalid(
            "The Devin model and messages are required.",
        ));
    }
    if value
        .get("stop")
        .is_some_and(|s| !s.is_null() && s != &json!([]) && s != &json!(""))
    {
        return Err(PluginError::invalid(
            "Devin's native protocol cannot apply stop sequences.",
        ));
    }
    if value
        .get("tool_choice")
        .is_some_and(|choice| !choice.is_null() && choice != "auto" && choice != "none")
    {
        return Err(PluginError::invalid(
            "Devin's native protocol cannot force a particular tool call.",
        ));
    }
    for (key, low, high) in [("temperature", 0., 2.), ("top_p", 0., 1.)] {
        if value.get(key).is_some_and(|v| {
            v.as_f64()
                .is_none_or(|n| !n.is_finite() || n < low || n > high)
        }) {
            return Err(PluginError::invalid(
                "The Devin sampling setting is unsupported.",
            ));
        }
    }
    Ok(value)
}
struct ResultImages {
    index: usize,
    count: usize,
    seen: usize,
    plain: String,
}
fn noted(text: &str) -> (usize, String) {
    if let Some(start) = text.rfind("[The tool returned ") {
        let tail = &text[start..];
        let count = if tail == "[The tool returned an image; it follows in the next message.]" {
            1
        } else if let Some(n) = tail
            .strip_prefix("[The tool returned ")
            .and_then(|t| t.strip_suffix(" images; they follow in the next message.]"))
        {
            n.parse::<usize>().unwrap_or(0)
        } else {
            0
        };
        if count > 0 && count <= 64 {
            return (count, text[..start].trim_end_matches('\n').into());
        }
    }
    (0, text.into())
}
fn settle(messages: &mut [Message], results: &mut BTreeMap<String, ResultImages>) {
    for result in results.values() {
        let msg = &mut messages[result.index];
        if result.count > 0 && result.seen == result.count {
            msg.text = result.plain.clone();
        }
        if msg.text.is_empty() {
            msg.text = "(no output)".into();
        }
    }
    results.clear();
}
pub(crate) fn messages(chat: &Value) -> PluginResult<(Vec<Message>, Vec<String>)> {
    let mut msgs = Vec::<Message>::new();
    let mut pending = Vec::<Call>::new();
    let mut system = Vec::new();
    let mut results = BTreeMap::<String, ResultImages>::new();
    let input = chat["messages"]
        .as_array()
        .ok_or_else(|| PluginError::invalid("Devin messages must be an array."))?;
    if input.len() > 100000 {
        return Err(PluginError::invalid("Too many Devin messages."));
    }
    for message in input {
        match message["role"].as_str() {
            Some("system" | "developer") => {
                let (text, images) = content(&message["content"])?;
                if !images.is_empty() {
                    return Err(unsupported());
                }
                system.push(text);
            }
            Some("assistant") => {
                settle(&mut msgs, &mut results);
                let (text, images) = content(&message["content"])?;
                if !images.is_empty() {
                    return Err(unsupported());
                }
                let mut calls = Vec::new();
                for c in message["tool_calls"].as_array().into_iter().flatten() {
                    let id = account::text(c.get("id"), 8192).ok_or_else(|| {
                        PluginError::invalid("Devin history needs the original tool call ID.")
                    })?;
                    let name = account::text(c.pointer("/function/name"), 256)
                        .ok_or_else(|| PluginError::invalid("A Devin history tool has no name."))?;
                    let raw = c
                        .pointer("/function/arguments")
                        .cloned()
                        .unwrap_or(json!({}));
                    let args = if let Some(raw) = raw.as_str() {
                        serde_json::from_str::<Value>(raw).map_err(|_| {
                            PluginError::invalid("A Devin history tool has invalid JSON arguments.")
                        })?;
                        raw.to_owned()
                    } else {
                        raw.to_string()
                    };
                    let native_id = in_id(id);
                    if calls.iter().any(|call: &Call| call.id == native_id) {
                        return Err(PluginError::invalid(
                            "A Devin history turn has duplicate tool call IDs.",
                        ));
                    }
                    calls.push(Call {
                        id: in_id(id),
                        name: name.into(),
                        args,
                    });
                }
                if text.is_empty() && calls.is_empty() {
                    continue;
                }
                let join = msgs
                    .last()
                    .is_some_and(|m| m.role == 2 && !m.calls.is_empty());
                if join {
                    let last = msgs.last_mut().unwrap();
                    last.text = [last.text.as_str(), text.as_str()]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    if calls
                        .iter()
                        .any(|call| last.calls.iter().any(|old| old.id == call.id))
                    {
                        return Err(PluginError::invalid(
                            "A Devin history turn has duplicate tool call IDs.",
                        ));
                    }
                    last.calls.extend(calls.clone());
                    pending.extend(calls);
                } else {
                    missing(&mut msgs, &mut pending);
                    let mut msg = Message::new(2, text);
                    msg.calls = calls.clone();
                    msgs.push(msg);
                    pending = calls;
                }
            }
            Some("tool") => {
                let original = account::text(message.get("tool_call_id"), 8192)
                    .ok_or_else(|| PluginError::invalid("A Devin tool result has no owner."))?;
                let id = in_id(original);
                let at = pending.iter().position(|c| c.id == id).ok_or_else(|| {
                    PluginError::invalid("A Devin tool result does not answer a pending call.")
                })?;
                let (text, images) = content(&message["content"])?;
                let (count, plain) = noted(&text);
                let mut msg = Message::new(4, text);
                msg.answer = id;
                msg.images = images;
                let index = msgs.len();
                msgs.push(msg);
                results.insert(
                    original.into(),
                    ResultImages {
                        index,
                        count,
                        seen: 0,
                        plain,
                    },
                );
                pending.remove(at);
            }
            Some("user") => {
                let mut text: Vec<String> = Vec::new();
                let mut images = Vec::new();
                let mut owner: Option<String> = None;
                if let Some(value) = message["content"].as_str() {
                    text.push(value.into());
                } else if let Some(parts) = message["content"].as_array() {
                    for part in parts {
                        match part["type"].as_str() {
                            Some("text") => {
                                let value = part["text"].as_str().ok_or_else(unsupported)?;
                                owner = results
                                    .iter()
                                    .find(|(id, result)| {
                                        result.seen < result.count
                                            && (value
                                                == format!("[From the result of tool call {id}:]")
                                                || value.starts_with("[From the result of ")
                                                    && value
                                                        .ends_with(&format!(" (tool call {id}):]")))
                                    })
                                    .map(|(id, _)| id.clone());
                                if owner.is_none() {
                                    text.push(value.into());
                                }
                            }
                            Some("image_url") => {
                                let image = image(part)?;
                                if let Some(result) =
                                    owner.as_ref().and_then(|id| results.get_mut(id))
                                {
                                    msgs[result.index].images.push(image);
                                    result.seen += 1;
                                    if result.seen >= result.count {
                                        owner = None;
                                    }
                                } else {
                                    images.push(image);
                                }
                            }
                            _ => return Err(unsupported()),
                        }
                    }
                } else {
                    return Err(unsupported());
                }
                settle(&mut msgs, &mut results);
                let text = text.join("\n\n");
                if !text.is_empty() || !images.is_empty() {
                    missing(&mut msgs, &mut pending);
                    let mut msg = Message::new(1, text);
                    msg.images = images;
                    msgs.push(msg);
                }
            }
            _ => {
                return Err(PluginError::invalid(
                    "This Devin message role is unsupported.",
                ))
            }
        }
    }
    settle(&mut msgs, &mut results);
    missing(&mut msgs, &mut pending);
    let format = format(&chat["response_format"]);
    if !format.is_empty() {
        system.push(format);
    }
    if msgs
        .last()
        .is_none_or(|m| m.role == 2 && m.calls.is_empty())
    {
        msgs.push(Message::new(1, "Please proceed with the task.".into()));
    }
    Ok((msgs, system))
}
pub(crate) fn build(chat: &Value, uid: &str, key: &str) -> PluginResult<Vec<u8>> {
    let (mut messages, mut system) = messages(chat)?;
    let mut tools = Vec::new();
    if chat["tool_choice"] != "none" {
        for tool in chat["tools"].as_array().into_iter().flatten() {
            if tool.get("type").is_some_and(|v| v != "function") {
                return Err(PluginError::invalid(
                    "Devin only accepts caller-defined function tools.",
                ));
            }
            let f = &tool["function"];
            let name = account::text(f.get("name"), 256)
                .ok_or_else(|| PluginError::invalid("The Devin tool name is invalid."))?;
            let schema = schema::object_root(
                f.get("parameters")
                    .unwrap_or(&json!({"type":"object","properties":{}})),
            )?;
            let schema = if uid.to_ascii_lowercase().contains("gemini") {
                schema::gemini(&schema, 0)?
            } else {
                schema
            };
            tools.push((
                name.to_owned(),
                f["description"].as_str().unwrap_or_default().to_owned(),
                schema,
            ));
        }
    }
    let described = tools
        .iter()
        .filter(|(_, d, _)| !d.is_empty())
        .map(|(n, d, _)| format!("<tool name=\"{n}\">\n{}\n</tool>\n", d.trim()))
        .collect::<String>();
    if !described.is_empty() {
        system.push(format!(
            "<tool_descriptions>\n{described}</tool_descriptions>"
        ));
    }
    let instructions = system
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n");
    if !instructions.is_empty() {
        let index = if let Some(index) = messages.iter().position(|m| m.role == 1) {
            index
        } else {
            messages.insert(0, Message::new(1, String::new()));
            0
        };
        messages[index].text = [instructions.as_str(), messages[index].text.as_str()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
    }
    let mut out = Pb::new().bytes(1, network::metadata(key));
    for message in &messages {
        out = out.bytes(3, message.encode());
    }
    let max = chat["max_completion_tokens"]
        .as_u64()
        .or_else(|| chat["max_tokens"].as_u64())
        .unwrap_or(128000);
    if max == 0 || max > 1000000 {
        return Err(PluginError::invalid(
            "The Devin output limit is unsupported.",
        ));
    }
    let sampling = |key: &str, default: f64| {
        let value = chat[key].as_f64().unwrap_or(default);
        if value == 0. {
            0.000001
        } else {
            value
        }
    };
    out = out.int(7, 5).bytes(
        8,
        Pb::new()
            .int(1, 1)
            .int(2, max)
            .int(3, 400)
            .double(5, sampling("temperature", 1.))
            .int(7, 40)
            .double(8, sampling("top_p", 0.95)),
    );
    let mut offered = BTreeSet::new();
    for (name, description, schema) in tools {
        offered.insert(name.clone());
        let description = if description.is_empty() {
            name.clone()
        } else {
            format!("Described under <tool name=\"{name}\"> in <tool_descriptions>, in the instructions.")
        };
        out = out.bytes(
            10,
            Pb::new()
                .text(1, &name)
                .text(2, &description)
                .text(3, &schema.to_string()),
        );
    }
    for message in &messages {
        for call in &message.calls {
            if offered.insert(call.name.clone()) {
                out = out.bytes(
                    10,
                    Pb::new()
                        .text(1, &call.name)
                        .text(2, "Tool")
                        .text(3, "{\"type\":\"object\",\"properties\":{}}"),
                );
            }
        }
    }
    Ok(out.text(21, uid).0)
}
#[cfg(test)]
#[path = "../test/request.test.rs"]
mod tests;
