use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};

pub(crate) struct Pin {
    key: &'static str,
    value: Value,
    pub fallback: bool,
}

impl Pin {
    pub fn for_chat(chat: &Value, preference: Option<bool>, images: bool) -> Option<Self> {
        if preference == Some(false)
            || chat.get("providerOptions").is_some()
            || chat.get("provider").is_some()
        {
            return None;
        }
        let model = chat["model"].as_str()?;
        if model
            .to_ascii_lowercase()
            .starts_with("cline-pass/deepseek")
        {
            return Some(Self {
                key: "providerOptions",
                value: json!({"gateway": {
                    "only": ["deepseek"],
                    "has": if images { vec!["vision", "implicit-caching"] } else { vec!["implicit-caching"] }
                }}),
                fallback: preference.is_none(),
            });
        }
        if preference != Some(true) || model.starts_with("cline-free/") {
            return None;
        }
        let name = model.rsplit('/').next()?.to_ascii_lowercase();
        let (key, value) = if name == "glm-5.3-flash" {
            ("provider", json!({"only": ["z-ai"]}))
        } else if name.starts_with("deepseek") {
            (
                "providerOptions",
                json!({"gateway": {"only": ["deepseek"]}}),
            )
        } else if name.starts_with("glm") {
            ("providerOptions", json!({"gateway": {"only": ["zai"]}}))
        } else {
            return None;
        };
        Some(Self {
            key,
            value,
            fallback: false,
        })
    }

    pub fn apply(&self, bytes: &[u8]) -> PluginResult<Vec<u8>> {
        let end = bytes
            .iter()
            .rposition(|byte| *byte == b'}')
            .ok_or_else(|| PluginError::invalid("The Cline chat request is invalid."))?;
        let previous = bytes[..end]
            .iter()
            .rfind(|byte| !byte.is_ascii_whitespace());
        let field = format!(
            "{}{}:{}",
            if matches!(previous, Some(b'{') | Some(b',')) {
                ""
            } else {
                ","
            },
            serde_json::to_string(self.key).unwrap(),
            self.value
        );
        // Append routing without reserializing the prefix matched by the upstream cache.
        let mut body = Vec::with_capacity(bytes.len() + field.len());
        body.extend_from_slice(&bytes[..end]);
        body.extend_from_slice(field.as_bytes());
        body.extend_from_slice(&bytes[end..]);
        Ok(body)
    }
}

pub(crate) fn preference(value: Option<&Value>) -> Option<bool> {
    match value {
        Some(Value::Bool(value)) => Some(*value),
        Some(Value::String(value)) if value == "true" => Some(true),
        Some(Value::String(value)) if value == "false" => Some(false),
        _ => None,
    }
}

pub(crate) fn has_images(bytes: &[u8]) -> bool {
    let Ok(body) = std::str::from_utf8(bytes) else {
        return false;
    };
    body.split("\"type\"").skip(1).any(|tail| {
        tail.trim_start().strip_prefix(':').is_some_and(|tail| {
            let tail = tail.trim_start();
            ["\"image_url\"", "\"image\"", "\"input_image\""]
                .iter()
                .any(|kind| tail.starts_with(kind))
        })
    })
}

pub(crate) fn unpin_worthy(status: u16, bytes: &[u8]) -> bool {
    if status >= 500 {
        return true;
    }
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    text.contains("no upstream")
        || ["matching", "available", "eligible"].iter().any(|kind| {
            ["upstream", "provider", "endpoint"]
                .iter()
                .any(|target| text.contains(&format!("no {kind} {target}")))
        })
}
