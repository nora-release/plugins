use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

pub(crate) fn key_id(key: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"zcode-device\0");
    hash.update(key.as_bytes());
    let mut bytes: [u8; 16] = hash.finalize()[..16].try_into().unwrap();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}

pub(crate) fn user_id(device: &str) -> String {
    format!(
        r#"{{"device_id":{},"account_uuid":"","session_id":""}}"#,
        serde_json::to_string(device).unwrap()
    )
}

fn is_js_whitespace(character: char) -> bool {
    // Match String.trim in ZCode's JavaScript client: BOM is blank, NEL is not.
    matches!(
        character,
        '\u{0009}'..='\u{000d}'
            | '\u{0020}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

// ZCode chooses its prompt cache by this device. A caller's nonempty user_id
// already makes that choice; keep its original body, including its formatting.
pub(crate) fn mark_coding_body(body: &mut Value, device: &str) -> bool {
    if device.is_empty() || !body.is_object() || !body["messages"].is_array() {
        return false;
    }
    if body["metadata"]["user_id"]
        .as_str()
        .is_some_and(|id| !id.chars().all(is_js_whitespace))
    {
        return false;
    }
    if !body["metadata"].is_object() {
        body["metadata"] = Value::Object(Map::new());
    }
    body["metadata"]["user_id"] = Value::String(user_id(device));
    true
}
