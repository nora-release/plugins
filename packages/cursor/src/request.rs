use super::{
    auth,
    models::Selection,
    protobuf::{self, Pb},
};
use base64::Engine;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
pub(crate) const NS: &str = "magpie";
#[derive(Clone, Debug)]
pub(crate) struct Tool {
    pub name: String,
    pub description: String,
    pub schema: Value,
}
pub(crate) fn uuid() -> String {
    let mut bytes = nora_ai::oauth_pkce::random_hex(16).into_bytes();
    bytes[12] = b'4';
    bytes[16] = b'8';
    let s = String::from_utf8(bytes).unwrap();
    format!(
        "{}-{}-{}-{}-{}",
        &s[..8],
        &s[8..12],
        &s[12..16],
        &s[16..20],
        &s[20..]
    )
}
pub(crate) fn tools(chat: &Value) -> PluginResult<Vec<Tool>> {
    if chat["tool_choice"] == "none" {
        return Ok(Vec::new());
    }
    let only = chat
        .pointer("/tool_choice/function/name")
        .and_then(Value::as_str);
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for t in chat["tools"].as_array().into_iter().flatten() {
        if t["type"] != "function" {
            return Err(PluginError::new(
                "unsupported_media_type",
                "Cursor supports only caller-advertised function tools.",
            ));
        }
        let name = auth::text(t.pointer("/function/name"), 128)
            .filter(|s| !s.chars().any(char::is_whitespace))
            .ok_or_else(|| PluginError::invalid("The Cursor tool name is invalid."))?;
        if !seen.insert(name) {
            return Err(PluginError::invalid(
                "Duplicate Cursor tool names are ambiguous.",
            ));
        }
        if only.is_some_and(|s| s != name) {
            continue;
        }
        let schema = t
            .pointer("/function/parameters")
            .cloned()
            .unwrap_or_else(|| json!({"type":"object","properties":{}}));
        if !schema.is_object() {
            return Err(PluginError::invalid("Cursor tools require object schemas."));
        }
        out.push(Tool {
            name: name.into(),
            description: t
                .pointer("/function/description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            schema,
        });
    }
    if only.is_some() && out.is_empty() {
        return Err(PluginError::invalid(
            "The selected Cursor tool was not advertised.",
        ));
    }
    if out.len() > 256 || serde_json::to_vec(&chat["tools"]).map_or(true, |v| v.len() > 1024 * 1024)
    {
        return Err(PluginError::invalid(
            "The Cursor tool catalog is too large.",
        ));
    }
    Ok(out)
}
pub(crate) fn text(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.into();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["type"] == "text")
        .filter_map(|p| p["text"].as_str())
        .collect::<Vec<_>>()
        .join("")
}
fn reasoning(message: &Value) -> String {
    ["reasoning_content", "reasoning", "reasoning_text"]
        .into_iter()
        .find_map(|k| message[k].as_str().filter(|s| !s.is_empty()))
        .unwrap_or_default()
        .into()
}
fn args(v: &Value) -> PluginResult<Value> {
    let value = if let Some(s) = v.as_str() {
        serde_json::from_str(s)
            .map_err(|_| PluginError::invalid("Cursor tool arguments must be valid JSON."))?
    } else {
        v.clone()
    };
    if value.is_object() {
        Ok(value)
    } else {
        Err(PluginError::invalid(
            "Cursor tool arguments must be an object.",
        ))
    }
}
fn unsupported() -> PluginError {
    PluginError::new("unsupported_media_type","Cursor accepts text and inline PNG, JPEG, GIF or WebP images; this attachment is unsupported.")
}
pub(crate) fn image(part: &Value) -> PluginResult<(String, Vec<u8>)> {
    let raw = part
        .pointer("/image_url/url")
        .and_then(Value::as_str)
        .or_else(|| part["image_url"].as_str())
        .ok_or_else(unsupported)?;
    let (mime, data) = raw
        .strip_prefix("data:")
        .and_then(|s| s.split_once(";base64,"))
        .ok_or_else(unsupported)?;
    if !["image/png", "image/jpeg", "image/gif", "image/webp"].contains(&mime)
        || data.len() > 24 * 1024 * 1024
    {
        return Err(unsupported());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| unsupported())?;
    if bytes.is_empty()
        || bytes.len() > 16 * 1024 * 1024
        || base64::engine::general_purpose::STANDARD.encode(&bytes) != data
    {
        return Err(unsupported());
    }
    Ok((mime.into(), bytes))
}
fn user_parts(value: &Value) -> PluginResult<Vec<Value>> {
    if let Some(s) = value.as_str() {
        return Ok(vec![json!({"type":"text","text":s})]);
    }
    let parts = value
        .as_array()
        .ok_or_else(|| PluginError::invalid("Cursor message content is invalid."))?;
    let mut out = Vec::new();
    for p in parts {
        match p["type"].as_str() {
            Some("text") => {
                let s = p["text"]
                    .as_str()
                    .ok_or_else(|| PluginError::invalid("Cursor text is invalid."))?;
                out.push(json!({"type":"text","text":s}));
            }
            Some("image_url") => {
                let (mime, bytes) = image(p)?;
                let hex = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
                out.push(json!({"type":"image","mimeType":mime,"image":{"__type":"Uint8Array","hex":hex}}));
            }
            _ => return Err(unsupported()),
        }
    }
    Ok(out)
}
fn tool_result(id: &str, value: &Value, error: bool) -> PluginResult<Value> {
    let parts = if value.is_string() {
        vec![json!({"type":"text","text":value.as_str().unwrap()})]
    } else {
        value.as_array().ok_or_else(unsupported)?.clone()
    };
    let mut content = Vec::new();
    for part in parts {
        match part["type"].as_str() {
            Some("text") => {
                let s = part["text"].as_str().ok_or_else(unsupported)?;
                content.push(json!({"type":"text","text":s}));
            }
            Some("image_url") => {
                let (mime, bytes) = image(&part)?;
                content.push(json!({"type":"image","data":base64::engine::general_purpose::STANDARD.encode(bytes),"mimeType":mime}));
            }
            _ => return Err(unsupported()),
        }
    }
    let text = content
        .iter()
        .filter_map(|v| v["text"].as_str())
        .collect::<Vec<_>>()
        .join("");
    let parsed = serde_json::from_str::<Value>(&text).unwrap_or_else(|_| json!(text));
    Ok(
        json!({"type":"tool-result","toolCallId":id,"toolName":"CallDynamicTool","result":parsed,"experimental_content":content,"isError":error}),
    )
}
pub(crate) struct Conversation {
    pub messages: Vec<Vec<u8>>,
    pub last: String,
    pub estimate: u64,
}
pub(crate) fn conversation(chat: &Value, tools: &[Tool]) -> PluginResult<Conversation> {
    let input = chat["messages"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 4096)
        .ok_or_else(|| PluginError::invalid("Cursor requires a nonempty message history."))?;
    let mut system = input
        .iter()
        .filter(|m| m["role"] == "system" || m["role"] == "developer")
        .map(|m| text(&m["content"]))
        .collect::<Vec<_>>()
        .join("\n\n");
    if !tools.is_empty() {
        system.push_str("\n\n<dynamic_tool_catalog>\nThe MCP namespace \"magpie\" has the following tools. Call each only through CallDynamicTool({\"namespace\":\"magpie\",\"toolName\":NAME,\"arguments\":ARGS}); never call it as a builtin.\n");
        for t in tools {
            system.push_str(&format!("<call>CallDynamicTool({{\"namespace\":\"magpie\",\"toolName\":{},\"arguments\":{{...}}}})\n{}\narguments schema: {}\n</call>\n",json!(t.name),t.description,t.schema));
        }
        system.push_str("</dynamic_tool_catalog>");
    }
    let mut messages = Vec::new();
    if !system.is_empty() {
        messages.push(json!({"role":"system","content":system}));
    }
    let mut pending = BTreeSet::new();
    let mut results = Vec::new();
    let mut last = ".".to_owned();
    let flush = |messages: &mut Vec<Value>,
                 pending: &mut BTreeSet<String>,
                 results: &mut Vec<Value>|
     -> PluginResult<()> {
        for id in pending.iter() {
            results.push(tool_result(
                id,
                &json!("Tool use was interrupted and did not produce a result."),
                true,
            )?);
        }
        pending.clear();
        if !results.is_empty() {
            messages.push(json!({"role":"tool","content":std::mem::take(results)}));
        }
        Ok(())
    };
    for m in input {
        let role = m["role"]
            .as_str()
            .ok_or_else(|| PluginError::invalid("The Cursor message role is invalid."))?;
        match role {
            "system" | "developer" => {}
            "tool" => {
                let id = auth::text(m.get("tool_call_id"), 512).ok_or_else(|| {
                    PluginError::invalid("Cursor tool results require their call ID.")
                })?;
                if !pending.remove(id) {
                    return Err(PluginError::invalid(
                        "A Cursor tool result is unmatched or duplicated.",
                    ));
                }
                results.push(tool_result(
                    id,
                    &m["content"],
                    m["is_error"] == true || m["isError"] == true,
                )?);
            }
            "assistant" => {
                // Consecutive assistant fragments can contain one parallel tool batch.
                let merge = results.is_empty()
                    && !pending.is_empty()
                    && messages.last().is_some_and(|m| m["role"] == "assistant");
                if !merge {
                    flush(&mut messages, &mut pending, &mut results)?;
                }
                let mut content = Vec::new();
                let thought = reasoning(m);
                if !thought.is_empty() {
                    content.push(json!({"type":"reasoning","text":thought}));
                }
                let message = text(&m["content"]);
                if !message.is_empty() {
                    content.push(json!({"type":"text","text":message}));
                }
                for c in m["tool_calls"].as_array().into_iter().flatten() {
                    if c.get("type").is_some_and(|v| v != "function") {
                        return Err(PluginError::invalid("Cursor only accepts function calls."));
                    }
                    let id = auth::text(c.get("id"), 512).ok_or_else(|| {
                        PluginError::invalid("Cursor tool calls require their original ID.")
                    })?;
                    if !pending.insert(id.into()) {
                        return Err(PluginError::invalid(
                            "Duplicate Cursor tool call IDs are ambiguous.",
                        ));
                    }
                    let name = auth::text(c.pointer("/function/name"), 128).ok_or_else(|| {
                        PluginError::invalid("The Cursor tool call name is invalid.")
                    })?;
                    content.push(json!({"type":"tool-call","toolCallId":id,"toolName":"CallDynamicTool","args":{"namespace":NS,"toolName":name,"arguments":args(&c["function"]["arguments"])?}}));
                }
                if !content.is_empty() {
                    if merge {
                        messages.last_mut().unwrap()["content"]
                            .as_array_mut()
                            .unwrap()
                            .extend(content);
                    } else {
                        messages.push(json!({"role":"assistant","content":content}));
                    }
                }
            }
            "user" => {
                flush(&mut messages, &mut pending, &mut results)?;
                let content = user_parts(&m["content"])?;
                if let Some(s) = content
                    .iter()
                    .find_map(|p| p["text"].as_str().filter(|s| !s.is_empty()))
                {
                    last = s.into();
                }
                messages.push(json!({"role":"user","content":content}));
            }
            _ => {
                return Err(PluginError::invalid(
                    "The Cursor message role is unsupported.",
                ))
            }
        }
    }
    flush(&mut messages, &mut pending, &mut results)?;
    let messages: Vec<Vec<u8>> = messages
        .iter()
        .map(serde_json::to_vec)
        .collect::<Result<_, _>>()
        .map_err(|_| PluginError::invalid("The Cursor history is invalid."))?;
    let size: usize = messages.iter().map(Vec::len).sum();
    if size > 32 * 1024 * 1024 {
        return Err(PluginError::invalid("The Cursor history is too large."));
    }
    Ok(Conversation {
        messages,
        last,
        estimate: (size / 4) as u64,
    })
}
pub(crate) fn definition(tool: &Tool) -> Pb {
    Pb::new()
        .str(1, &tool.name)
        .str(2, &tool.description)
        .bytes(3, protobuf::value(&tool.schema))
        .str(4, NS)
        .str(5, &tool.name)
        .str(6, &tool.schema.to_string())
}
pub(crate) fn environment() -> Pb {
    Pb::new()
        .str(
            1,
            if cfg!(target_os = "macos") {
                "darwin"
            } else {
                std::env::consts::OS
            },
        )
        .str(2, "/tmp")
        .str(10, "UTC")
}
pub(crate) fn build(
    conv: &Conversation,
    tools: &[Tool],
    model: &Selection,
    cid: &str,
) -> PluginResult<(Vec<u8>, BTreeMap<Vec<u8>, Vec<u8>>)> {
    use sha2::{Digest, Sha256};
    let mut blobs = BTreeMap::new();
    let mut put = |bytes: Vec<u8>| {
        let key = Sha256::digest(&bytes).to_vec();
        blobs.insert(key.clone(), bytes);
        key
    };
    let mut state = Pb::new();
    for m in &conv.messages {
        state = state.bytes(1, put(m.clone()));
    }
    let mid = uuid();
    let user = Pb::new().str(1, &conv.last).str(2, &mid).num(4, 1);
    let turn = Pb::new().bytes(1, Pb::new().bytes(1, put(user.0)).str(10, &mid));
    state = state.bytes(8, put(turn.0)).num(10, 1).str(22, "cli");
    let mut rc = Pb::new().bytes(4, environment());
    let mut mcp = Pb::new();
    for tool in tools {
        let d = definition(tool);
        rc = rc.bytes(7, &d);
        mcp = mcp.bytes(1, d);
    }
    let action = Pb::new().bytes(2, Pb::new().bytes(2, rc));
    let mut details = Pb::new()
        .str(1, &model.id)
        .str(3, &model.id)
        .str(4, &model.id);
    let mut requested = Pb::new().str(1, &model.id);
    if model.max_mode {
        details = details.num(7, 1);
        requested = requested.num(2, 1);
    }
    for (id, value) in &model.params {
        requested = requested.bytes(3, Pb::new().str(1, id).str(2, value));
    }
    let first = Pb::new().bytes(
        1,
        Pb::new()
            .bytes(1, state)
            .bytes(2, action)
            .bytes(3, details)
            .bytes(4, mcp)
            .str(5, cid)
            .bytes(9, requested)
            .num(19, 1),
    );
    Ok((first.0, blobs))
}
pub(crate) fn selector(chat: &Value, metadata: &BTreeMap<String, String>, account: &str) -> String {
    let session = chat["prompt_cache_key"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| metadata.get("sessionID").map(String::as_str))
        .unwrap_or_default();
    if session.is_empty() {
        return String::new();
    }
    let first = chat["messages"]
        .as_array()
        .and_then(|m| m.iter().find(|m| m["role"] == "user"));
    auth::hash(format!(
        "{account}\0{session}\0{}",
        first.unwrap_or(&Value::Null)
    ))
}
pub(crate) fn native_result(message: &Value) -> PluginResult<Vec<u8>> {
    let parts = if let Some(text) = message["content"].as_str() {
        vec![json!({"type":"text","text":text})]
    } else {
        message["content"]
            .as_array()
            .ok_or_else(unsupported)?
            .clone()
    };
    let mut success = Pb::new();
    let mut size = 0;
    for part in parts {
        match part["type"].as_str() {
            Some("text") => {
                let text = part["text"].as_str().ok_or_else(unsupported)?;
                size += text.len();
                success = success.bytes(1, Pb::new().bytes(1, Pb::new().str(1, text)));
            }
            Some("image_url") => {
                let (mime, bytes) = image(&part)?;
                size += bytes.len();
                success = success.bytes(
                    1,
                    Pb::new().bytes(2, Pb::new().bytes(1, bytes).str(2, &mime)),
                );
            }
            _ => return Err(unsupported()),
        }
        if size > 16 * 1024 * 1024 {
            return Err(unsupported());
        }
    }
    if message["is_error"] == true || message["isError"] == true {
        success = success.num(2, 1);
    }
    Ok(Pb::new().bytes(1, success).0)
}

#[cfg(test)]
#[path = "../test/request.test.rs"]
mod tests;
