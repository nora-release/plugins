use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
pub(crate) const BUILDER_PROFILE: &str =
    "arn:aws:codewhisperer:us-east-1:638616132270:profile/AAAACCCCXXXX";
pub(crate) const BASE: &str = "https://runtime.us-east-1.kiro.dev";
#[derive(Clone, Default)]
pub(crate) struct Credential {
    pub access: String,
    pub refresh: String,
    pub expires: i64,
    pub method: String,
    pub region: String,
    pub profile: String,
    pub client_id: String,
    pub client_secret: String,
    pub builder: bool,
    pub email: String,
    pub plan: String,
    pub identity: String,
}
pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
pub(crate) fn fingerprint(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
pub(crate) fn text(value: Option<&Value>, max: usize) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| {
            !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
        })
}
pub(crate) fn expired() -> PluginError {
    PluginError::authentication("The Kiro account is unavailable or changed. Sign in again or refresh the selected source in Kiro.")
}
pub(crate) fn secret(value: Option<&Value>) -> PluginResult<String> {
    text(value, 64 * 1024)
        .filter(|value| !value.chars().any(char::is_whitespace))
        .map(str::to_owned)
        .ok_or_else(expired)
}
pub(crate) fn region(value: &str) -> PluginResult<String> {
    let parts = value.split('-').collect::<Vec<_>>();
    if value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || parts.len() < 3
        || !parts[0].bytes().all(|byte| byte.is_ascii_lowercase())
        || parts[0].len() != 2
        || !parts[1..parts.len() - 1]
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_lowercase()))
        || !value
            .rsplit('-')
            .next()
            .is_some_and(|part| part.parse::<u8>().is_ok())
    {
        return Err(PluginError::invalid("The Kiro account region is invalid."));
    }
    Ok(value.into())
}
pub(crate) fn timestamp(value: Option<&Value>) -> i64 {
    let Some(value) = value else {
        return 0;
    };
    value
        .as_i64()
        .map(|value| {
            if value < 100_000_000_000 {
                value.saturating_mul(1000)
            } else {
                value
            }
        })
        .or_else(|| {
            value
                .as_str()
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .map(|time| time.timestamp_millis())
        })
        .unwrap_or(0)
}
impl Credential {
    pub fn api_key(value: &Value) -> PluginResult<Self> {
        Ok(Self {
            access: secret(value.get("apiKey").or_else(|| value.get("key")))?,
            method: "apikey".into(),
            region: "us-east-1".into(),
            ..Default::default()
        })
    }
    pub fn from_saved(value: &Value) -> PluginResult<Self> {
        let method = text(value.get("method"), 64).unwrap_or("social");
        if !["apikey", "social", "idc", "external-idp"].contains(&method) {
            return Err(expired());
        }
        Ok(Self {
            access: secret(value.get("access").or_else(|| value.get("key")))?,
            refresh: text(value.get("refresh"), 64 * 1024)
                .unwrap_or_default()
                .into(),
            expires: timestamp(value.get("expires")),
            method: method.into(),
            region: region(text(value.get("region"), 64).unwrap_or("us-east-1"))?,
            profile: text(value.get("profileArn"), 2048)
                .unwrap_or_default()
                .into(),
            client_id: text(value.get("clientId"), 4096).unwrap_or_default().into(),
            client_secret: text(value.get("clientSecret"), 16 * 1024)
                .unwrap_or_default()
                .into(),
            builder: value["builderID"] == true
                || value["loginProvider"]
                    .as_str()
                    .is_some_and(|provider| provider.eq_ignore_ascii_case("builderid")),
            email: text(value.get("email"), 512).unwrap_or_default().into(),
            plan: text(value.get("plan"), 128).unwrap_or_default().into(),
            identity: text(value.get("accountId"), 128).unwrap_or_default().into(),
        })
    }
    pub fn saved(&self) -> Value {
        json!({"type":if self.method=="apikey"{"api"}else{"oauth"},"authMethod":if self.method=="apikey"{"apiKey"}else{"oauth"},"method":self.method,"access":self.access,"refresh":self.refresh,"expires":if self.method=="apikey"{Value::Null}else{json!(self.expires)},"region":self.region,"profileArn":self.profile,"clientId":self.client_id,"clientSecret":self.client_secret,"builderID":self.builder,"accountId":self.identity,"email":self.email,"plan":self.plan,"label":if self.email.is_empty(){"Kiro account"}else{&self.email}})
    }
    pub fn runtime_region(&self) -> PluginResult<String> {
        let parts = self.profile.split(':').collect::<Vec<_>>();
        if parts.len() >= 6
            && parts[0] == "arn"
            && parts[1] == "aws"
            && parts[2] == "codewhisperer"
            && !parts[3].is_empty()
        {
            return region(parts[3]);
        }
        Ok(if self.region.starts_with("eu-") {
            "eu-central-1"
        } else {
            "us-east-1"
        }
        .into())
    }
    pub fn token_type(&self) -> Option<&str> {
        match self.method.as_str() {
            "apikey" => Some("API_KEY"),
            "external-idp" => Some("EXTERNAL_IDP"),
            _ => None,
        }
    }
    pub fn fresh(&self) -> bool {
        self.method == "apikey" || self.expires > now() + 120_000
    }
}
pub(crate) fn read_source(kind: &str, path: &str) -> PluginResult<Credential> {
    let path = Path::new(path);
    if !path.is_absolute() || !std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
        return Err(expired());
    }
    match kind {
        "cli" => {
            let db = rusqlite::Connection::open_with_flags(
                path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|_| expired())?;
            db.busy_timeout(std::time::Duration::from_secs(3))
                .map_err(|_| expired())?;
            let read = |key: &str| -> Option<Value> {
                let value: String = db
                    .query_row("SELECT value FROM auth_kv WHERE key = ?1", [key], |row| {
                        row.get(0)
                    })
                    .ok()?;
                if value.len() > 1024 * 1024 {
                    return None;
                }
                serde_json::from_str(&value).ok()
            };
            for (method, key) in [
                ("social", "kirocli:social:token"),
                ("idc", "kirocli:odic:token"),
                ("external-idp", "kirocli:external-idp:token"),
            ] {
                let Some(value) =
                    read(key).filter(|value| text(value.get("access_token"), 64 * 1024).is_some())
                else {
                    continue;
                };
                let mut c = Credential {
                    access: secret(value.get("access_token"))?,
                    refresh: text(value.get("refresh_token"), 64 * 1024)
                        .unwrap_or_default()
                        .into(),
                    expires: timestamp(value.get("expires_at")),
                    method: method.into(),
                    region: region(text(value.get("region"), 64).unwrap_or("us-east-1"))?,
                    profile: text(value.get("profile_arn"), 2048)
                        .unwrap_or_default()
                        .into(),
                    ..Default::default()
                };
                if method == "idc" {
                    c.builder = value.get("start_url").is_none()
                        || value["start_url"] == "https://view.awsapps.com/start";
                    if let Some(reg) = read("kirocli:odic:device-registration") {
                        c.client_id = text(reg.get("client_id"), 4096).unwrap_or_default().into();
                        c.client_secret = text(reg.get("client_secret"), 16 * 1024)
                            .unwrap_or_default()
                            .into();
                    }
                }
                return Ok(c);
            }
            Err(expired())
        }
        "ide" => {
            let value = read_json(path)?;
            let mut c = Credential {
                access: secret(value.get("accessToken"))?,
                refresh: text(value.get("refreshToken"), 64 * 1024)
                    .unwrap_or_default()
                    .into(),
                expires: timestamp(value.get("expiresAt")),
                method: "idc".into(),
                region: region(text(value.get("region"), 64).unwrap_or("us-east-1"))?,
                profile: text(value.get("profileArn"), 2048)
                    .unwrap_or_default()
                    .into(),
                builder: value["provider"]
                    .as_str()
                    .is_some_and(|value| value.eq_ignore_ascii_case("builderid")),
                ..Default::default()
            };
            if let Some(hash) = text(value.get("clientIdHash"), 128) {
                if !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(expired());
                }
                let registration = read_json(
                    &path
                        .parent()
                        .ok_or_else(expired)?
                        .join(format!("{hash}.json")),
                )?;
                c.client_id = text(registration.get("clientId"), 4096)
                    .unwrap_or_default()
                    .into();
                c.client_secret = text(registration.get("clientSecret"), 16 * 1024)
                    .unwrap_or_default()
                    .into();
            }
            if value["authMethod"]
                .as_str()
                .is_some_and(|method| method.eq_ignore_ascii_case("social"))
                || c.client_id.is_empty()
            {
                c.method = "social".into();
            }
            Ok(c)
        }
        _ => Err(expired()),
    }
}
fn read_json(path: &Path) -> PluginResult<Value> {
    let file = std::fs::File::open(path).map_err(|_| expired())?;
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| expired())?;
    if bytes.len() > 1024 * 1024 {
        return Err(expired());
    }
    serde_json::from_slice(&bytes).map_err(|_| expired())
}
#[cfg(test)]
#[path = "../test/session.test.rs"]
mod tests;
