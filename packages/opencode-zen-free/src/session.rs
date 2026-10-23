use nora_provider_plugin_sdk::FetchRequest;
use rand::RngCore;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Mutex,
};
#[derive(Default)]
pub(crate) struct Sessions {
    clock: Mutex<(u64, u16)>,
    sessions: Mutex<(BTreeMap<String, String>, VecDeque<String>)>,
}
impl Sessions {
    pub(crate) fn id(&self, prefix: &str, descending: bool) -> String {
        let now = nora_provider_subscription_core::now();
        let mut clock = self.clock.lock().unwrap_or_else(|error| error.into_inner());
        if clock.0 != now {
            *clock = (now, 0);
        }
        clock.1 = clock.1.wrapping_add(1);
        let sequence = if descending { clock.1.min(15) } else { clock.1 };
        let number = now.wrapping_mul(4096).wrapping_add(sequence as u64);
        let number = if descending { !number } else { number } & 0xffffffffffff;
        let alphabet = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
        let mut bytes = [0u8; 14];
        rand::thread_rng().fill_bytes(&mut bytes);
        let tail: String = bytes
            .iter()
            .map(|byte| alphabet[*byte as usize % 62] as char)
            .collect();
        format!("{prefix}_{number:012x}{tail}")
    }
    pub(crate) fn for_request(&self, request: &FetchRequest, body: &Value) -> String {
        let signal = request
            .metadata
            .get("sessionID")
            .cloned()
            .or_else(|| {
                [
                    "x-opencode-session",
                    "x-opencode-session-id",
                    "x-session-affinity",
                    "x-session-id",
                    "conversation-id",
                ]
                .iter()
                .find_map(|name| {
                    request
                        .headers
                        .iter()
                        .find(|(key, value)| key.eq_ignore_ascii_case(name) && !value.is_empty())
                        .map(|(_, value)| value.to_owned())
                })
            })
            .or_else(|| body["metadata"]["session_id"].as_str().map(str::to_owned))
            .or_else(|| {
                let messages = body.get("messages").or_else(|| body.get("input"))?;
                let first = if let Some(messages) = messages.as_array() {
                    messages
                        .iter()
                        .find(|message| message["role"] == "user")?
                        .get("content")?
                } else {
                    messages
                };
                if first.is_null() {
                    None
                } else {
                    Some(first.to_string())
                }
            });
        let Some(signal) = signal else {
            return self.id("ses", true);
        };
        if real(&signal) {
            return signal;
        }
        let digest = hex::encode(Sha256::digest(format!("ses\0{signal}").as_bytes()));
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let id = sessions
            .0
            .get(&digest)
            .cloned()
            .unwrap_or_else(|| self.id("ses", true));
        sessions.1.retain(|key| key != &digest);
        if sessions.0.len() >= 1000 && !sessions.0.contains_key(&digest) {
            if let Some(first) = sessions.1.pop_front() {
                sessions.0.remove(&first);
            }
        }
        sessions.0.insert(digest.clone(), id.clone());
        sessions.1.push_back(digest);
        id
    }
}
pub(crate) fn real(value: &str) -> bool {
    let Some(value) = value.strip_prefix("ses_") else {
        return false;
    };
    value.len() == 26
        && value.as_bytes()[..12]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        && value.as_bytes()[12..].iter().all(u8::is_ascii_alphanumeric)
        && u16::from_str_radix(&value[9..12], 16).is_ok_and(|counter| 0xfff - counter < 16)
}
#[cfg(test)]
#[path = "../test/session.test.rs"]
mod tests;
