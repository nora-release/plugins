use crate::{auth, definition::Definition};
use nora_provider_plugin_sdk::FetchRequest;
use serde_json::Value;
use sha2::{Digest, Sha256};

const RESERVED: [&str; 11] = [
    "model",
    "stream",
    "messages",
    "input",
    "contents",
    "system",
    "systemInstruction",
    "system_instruction",
    "instructions",
    "tools",
    "",
];

pub(crate) fn session(request: &FetchRequest, body: Option<&[u8]>) -> String {
    if let Some(id) = request
        .metadata
        .get("sessionID")
        .or_else(|| request.metadata.get("header:session-id"))
    {
        return id.clone();
    }
    let parsed = body
        .and_then(|body| serde_json::from_slice::<Value>(body).ok())
        .unwrap_or(Value::Null);
    let first = ["messages", "input", "contents"]
        .into_iter()
        .find_map(|key| {
            parsed.get(key).and_then(Value::as_array).and_then(|items| {
                items
                    .iter()
                    .find(|item| item.get("role").and_then(Value::as_str) == Some("user"))
            })
        })
        .or_else(|| parsed.get("input"));
    let mut first = first.cloned().unwrap_or(Value::Null);
    first.sort_all_objects();
    let digest = Sha256::digest(serde_json::to_vec(&first).unwrap_or_default());
    format!(
        "nora-{}",
        digest[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

pub(crate) fn apply(
    definition: &Definition,
    credentials: &Value,
    path: &str,
    session: &str,
    body: &mut Option<Vec<u8>>,
) {
    if !(path.ends_with("/chat/completions")
        || path.ends_with("/responses")
        || path.ends_with("/messages")
        || path.ends_with(":generateContent")
        || path.ends_with(":streamGenerateContent"))
    {
        return;
    }
    let codex = definition.config.oauth_provider.as_deref() == Some("openai-codex");
    if !auth::is_api(credentials) && !codex {
        return;
    }
    let Some(extra) = definition
        .config
        .extra
        .get("extraBody")
        .and_then(Value::as_object)
        .filter(|extra| !extra.is_empty())
    else {
        return;
    };
    let Some(mut parsed) = body
        .as_deref()
        .and_then(|body| serde_json::from_slice::<Value>(body).ok())
        .filter(Value::is_object)
    else {
        return;
    };
    let fields = parsed.as_object_mut().expect("Object checked above");
    let mut changed = false;
    for (key, value) in extra {
        let key = key.trim();
        let forced = key.starts_with('!');
        let name = key.strip_prefix('!').unwrap_or(key).trim();
        if RESERVED.contains(&name)
            || !auth::is_api(credentials) && matches!(name, "store" | "tool_choice" | "include")
            || !forced && fields.contains_key(name)
        {
            continue;
        }
        fields.insert(name.into(), with_session(value.clone(), session));
        changed = true;
    }
    if changed {
        if let Ok(encoded) = serde_json::to_vec(&parsed) {
            *body = Some(encoded);
        }
    }
}

fn with_session(value: Value, session: &str) -> Value {
    match value {
        Value::String(value) => Value::String(value.replace("${sessionId}", session)),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| with_session(value, session))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, with_session(value, session)))
                .collect(),
        ),
        value => value,
    }
}

#[cfg(test)]
#[path = "../test/extra_body.test.rs"]
mod tests;
