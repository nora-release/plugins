use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(super) fn conversation_key(instructions: &str, input: &[Value]) -> Option<String> {
    let mut hash = Sha256::new();
    hash.update(instructions.as_bytes());
    for item in input {
        hash.update(canonical_json(item).as_bytes());
        if item.get("role").and_then(Value::as_str) == Some("user") {
            let digest = format!("{:x}", hash.finalize());
            return Some(format!(
                "{}-{}-{}-{}-{}",
                &digest[..8],
                &digest[8..12],
                &digest[12..16],
                &digest[16..20],
                &digest[20..32]
            ));
        }
    }
    None
}

pub(super) fn bound_call_id(id: &str) -> String {
    if id.len() <= 64 {
        return id.into();
    }
    format!("{:x}", Sha256::digest(id.trim().as_bytes()))
}

// Match encoding/json's sorted object keys and escaping, including non-ASCII
// line separators, so an existing conversation keeps its key across clients.
pub(super) fn canonical_json(value: &Value) -> String {
    let mut value = value.clone();
    value.sort_all_objects();
    serde_json::to_string(&value)
        .unwrap_or_default()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

pub(super) fn apply_headers(headers: &mut BTreeMap<String, String>, body: &Value) {
    headers.retain(|name, _| {
        !["session_id", "conversation_id", "accept"].contains(&name.to_ascii_lowercase().as_str())
    });
    headers.insert("accept".into(), "text/event-stream".into());
    let Some(key) = body
        .get("prompt_cache_key")
        .and_then(Value::as_str)
        .filter(|key| !key.is_empty() && key.len() <= 16_384 && !key.chars().any(char::is_control))
    else {
        return;
    };
    headers.retain(|name, _| {
        !["session-id", "thread-id"].contains(&name.to_ascii_lowercase().as_str())
    });
    headers.insert("session-id".into(), key.into());
    headers.insert("thread-id".into(), key.into());
}
