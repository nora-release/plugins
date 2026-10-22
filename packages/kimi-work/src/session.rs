use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

pub(super) struct Session {
    pub work_token: String,
    pub membership_token: String,
    pub uid: String,
}

fn expired() -> PluginError {
    PluginError::authentication(
        "The selected Kimi Work sign-in is unavailable or changed. Reconnect the account.",
    )
}

fn token(value: Option<&Value>) -> PluginResult<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 64 * 1024
                && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        })
        .map(str::to_owned)
        .ok_or_else(expired)
}

fn uid(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

pub(super) fn read_selected(path: &Path) -> PluginResult<Session> {
    if !path.is_absolute() {
        return Err(PluginError::invalid(
            "Select an absolute path to Kimi desktop's config.json.",
        ));
    }
    let mut file = std::fs::File::open(path).map_err(|_| expired())?;
    if !file.metadata().map_err(|_| expired())?.is_file() {
        return Err(expired());
    }
    let mut bytes = Vec::new();
    use std::io::Read;
    file.by_ref()
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| expired())?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(expired());
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    let value: Value = serde_json::from_slice(bytes).map_err(|_| expired())?;
    if value
        .pointer("/credentials/kimiCode/baseUrl")
        .and_then(Value::as_str)
        .is_none_or(|url| url.trim_end_matches('/') != super::BASE)
    {
        return Err(expired());
    }
    let user = value
        .pointer("/credentials/kimiWeb/userId")
        .and_then(Value::as_str)
        .filter(|user| {
            !user.is_empty() && user.len() <= 4096 && !user.chars().any(char::is_control)
        })
        .ok_or_else(expired)?;
    Ok(Session {
        work_token: token(value.pointer("/credentials/kimiCode/apiKey"))?,
        membership_token: token(value.pointer("/credentials/kimiWeb/accessToken"))?,
        uid: uid(format!("kimi-work-account\0{user}").as_bytes()),
    })
}

pub(super) fn pasted(inputs: &Value) -> PluginResult<Session> {
    let work_token = token(inputs.get("workToken"))?;
    let membership_token = token(inputs.get("membershipToken"))?;
    let uid = uid(format!("kimi-work-pasted\0{work_token}\0{membership_token}").as_bytes());
    Ok(Session {
        work_token,
        membership_token,
        uid,
    })
}

pub(super) fn credentials(session: &Session, selected_path: Option<&str>) -> Value {
    if let Some(path) = selected_path {
        return json!({"type":"kimiWorkDesktop","authMethod":"desktop","label":"Kimi Work (selected desktop)",
            "source":{"kind":"kimi-work-desktop","configPath":path,"uid":session.uid}});
    }
    json!({"type":"oauth","authMethod":"tokens","label":"Kimi Work","access":session.work_token,"membershipToken":session.membership_token})
}

pub(super) fn current(credentials: Option<&Value>) -> PluginResult<Session> {
    let credentials = credentials.ok_or_else(expired)?;
    if let Some(source) = credentials.get("source") {
        if source["kind"] != "kimi-work-desktop" {
            return Err(expired());
        }
        let path = source
            .get("configPath")
            .and_then(Value::as_str)
            .ok_or_else(expired)?;
        let session = read_selected(Path::new(path))?;
        if source.get("uid").and_then(Value::as_str) != Some(session.uid.as_str()) {
            return Err(expired());
        }
        return Ok(session);
    }
    let work_token = token(credentials.get("access"))?;
    let membership_token = token(credentials.get("membershipToken"))?;
    Ok(Session {
        work_token,
        membership_token,
        uid: String::new(),
    })
}

#[cfg(test)]
#[path = "../test/session.test.rs"]
mod tests;
