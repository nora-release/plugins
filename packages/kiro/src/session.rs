use crate::source_kind::SourceKind;
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
    pub token_endpoint: String,
    pub builder: bool,
    pub email: String,
    pub user_id: String,
    pub realm: String,
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
pub(crate) fn realm(value: &str) -> PluginResult<String> {
    if value.trim().is_empty() {
        return Ok(String::new());
    }
    if value.len() > 2048 || value.chars().any(char::is_control) {
        return Err(expired());
    }
    let mut url = url::Url::parse(value).map_err(|_| expired())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(expired());
    }
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&path);
    let value = url.as_str().trim_end_matches('/');
    if value == "https://view.awsapps.com/start" {
        return Ok(String::new());
    }
    Ok(value.into())
}
fn realm_value(value: Option<&Value>) -> PluginResult<String> {
    match value {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(value)) => realm(value),
        _ => Err(expired()),
    }
}
pub(crate) fn token_endpoint(value: &str) -> PluginResult<String> {
    if value.len() > 2048 || value.chars().any(char::is_whitespace) {
        return Err(expired());
    }
    let url = url::Url::parse(value).map_err(|_| expired())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(expired());
    }
    Ok(url.into())
}
fn external_realm(endpoint: &str) -> PluginResult<String> {
    let mut url = url::Url::parse(&token_endpoint(endpoint)?).map_err(|_| expired())?;
    // Keep tenant query distinctions without copying a possible secret query
    // into Nora's source metadata. The request still uses the full endpoint.
    let query = url.query().map(fingerprint);
    url.set_query(None);
    if let Some(query) = query {
        let path = format!(
            "{}/.nora-query-scope-{query}",
            url.path().trim_end_matches('/')
        );
        url.set_path(&path);
    }
    realm(url.as_str())
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
        .or_else(|| {
            value
                .as_f64()
                .filter(|value| value.is_finite() && *value > 0.0 && *value < i64::MAX as f64)
                .map(|value| value.trunc() as i64)
        })
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
        let method = text(value.get("method"), 64).unwrap_or_else(|| {
            if value["type"]
                .as_str()
                .is_some_and(|kind| ["api", "apiKey"].contains(&kind))
                || value["authMethod"] == "apiKey"
            {
                "apikey"
            } else {
                "social"
            }
        });
        if !["apikey", "social", "idc", "external-idp"].contains(&method) {
            return Err(expired());
        }
        let credential = Self {
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
            token_endpoint: match value.get("tokenEndpoint") {
                None | Some(Value::Null) => String::new(),
                Some(Value::String(value)) if value.is_empty() => String::new(),
                Some(Value::String(value)) => token_endpoint(value)?,
                _ => return Err(expired()),
            },
            builder: value["builderID"] == true
                || value["loginProvider"]
                    .as_str()
                    .is_some_and(|provider| provider.eq_ignore_ascii_case("builderid")),
            email: text(value.get("email"), 512).unwrap_or_default().into(),
            user_id: text(value.get("userId"), 500)
                .or_else(|| {
                    value
                        .get("accountIdentity")
                        .or_else(|| value.pointer("/extras/accountIdentity"))
                        .filter(|identity| identity["issuer"] == "https://kiro.dev")
                        .and_then(|identity| text(identity.get("subject"), 512))
                        .and_then(|subject| subject.strip_prefix("user:"))
                })
                .unwrap_or_default()
                .into(),
            realm: if value["builderID"] == true {
                String::new()
            } else {
                realm_value(value.get("realm"))?
            },
            plan: text(value.get("plan"), 128).unwrap_or_default().into(),
            identity: text(value.get("accountId"), 128).unwrap_or_default().into(),
        };
        let identity = credential.account_identity();
        for previous in [
            value.get("accountIdentity"),
            value.pointer("/extras/accountIdentity"),
        ]
        .into_iter()
        .flatten()
        {
            if Some(previous) != identity.as_ref() {
                return Err(expired());
            }
        }
        Ok(credential)
    }
    pub fn saved(&self) -> Value {
        let mut value = json!({"type":if self.method=="apikey"{"api"}else{"oauth"},"authMethod":if self.method=="apikey"{"apiKey"}else{"oauth"},"method":self.method,"access":self.access,"refresh":self.refresh,"expires":if self.method=="apikey"{Value::Null}else{json!(self.expires)},"region":self.region,"profileArn":self.profile,"clientId":self.client_id,"clientSecret":self.client_secret,"tokenEndpoint":self.token_endpoint,"builderID":self.builder,"accountId":self.identity,"email":self.email,"userId":self.user_id,"realm":self.realm,"plan":self.plan,"label":if self.email.is_empty(){"Kiro account"}else{&self.email}});
        if let Some(identity) = self.account_identity() {
            value["accountIdentity"] = identity.clone();
            value["extras"]["accountIdentity"] = identity;
        }
        value
    }
    pub fn account_identity(&self) -> Option<Value> {
        let profile = self.profile.as_str();
        if profile.is_empty()
            || profile.trim() != profile
            || profile.len() > 2048
            || profile.chars().any(char::is_control)
        {
            return None;
        }
        let realm = if self.builder {
            String::new()
        } else {
            realm(&self.realm).ok()?
        };
        let subject = if !self.user_id.is_empty() {
            let id = self.user_id.trim();
            if id.is_empty()
                || id != self.user_id
                || id.len() > 500
                || id.chars().any(char::is_whitespace)
                || id.chars().any(char::is_control)
            {
                return None;
            }
            format!("user:{id}")
        } else {
            let email = self.email.trim().to_ascii_lowercase();
            if email.len() > 320
                || !email.contains('@')
                || email.chars().any(char::is_whitespace)
                || email.chars().any(char::is_control)
            {
                return None;
            }
            format!("email:{email}")
        };
        let scope = format!(
            "kiro-profile:{}",
            fingerprint(&if self.user_id.is_empty() {
                format!("{profile}\0{realm}")
            } else {
                profile.into()
            })
        );
        Some(json!({"issuer":"https://kiro.dev","subject":subject,"scope":scope}))
    }
    pub fn legacy_identity(&self) -> String {
        fingerprint(&format!(
            "kiro-account\0{}\0{}",
            self.email.to_ascii_lowercase(),
            if self.email.is_empty() {
                &self.profile
            } else {
                ""
            }
        ))
    }
    pub fn matches_saved_identity(&self, saved: &Value) -> bool {
        let Some(current) = self.account_identity() else {
            return false;
        };
        if matches!((saved.get("accountIdentity"),saved.pointer("/extras/accountIdentity")),(Some(a),Some(b)) if a!=b)
        {
            return false;
        }
        if let Some(previous) = saved
            .get("accountIdentity")
            .or_else(|| saved.pointer("/extras/accountIdentity"))
        {
            if previous == &current {
                return true;
            }
            // A previously verified email can be upgraded when Kiro begins
            // returning a user id, but an established user id is never weakened.
            let mut email_identity = self.clone();
            email_identity.user_id.clear();
            return !self.user_id.is_empty()
                && email_identity.account_identity().as_ref() == Some(previous)
                && text(saved.get("email"), 512)
                    .is_some_and(|email| email.eq_ignore_ascii_case(&self.email));
        }
        if self.email.is_empty()
            || !text(saved.get("accountId"), 128)
                .is_some_and(|identity| identity == self.legacy_identity())
            || !text(saved.get("email"), 512)
                .is_some_and(|email| email.eq_ignore_ascii_case(&self.email))
        {
            return false;
        }
        if text(saved.get("profileArn"), 2048).is_some_and(|profile| profile != self.profile) {
            return false;
        }
        if let Some(previous) = saved.get("realm") {
            return previous.as_str().and_then(|value| realm(value).ok())
                == realm(&self.realm).ok();
        }
        true
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
pub(crate) fn read_source(kind: &SourceKind, path: &str) -> PluginResult<Credential> {
    read_source_if_signed_in(kind, path)?.ok_or_else(expired)
}
pub(crate) fn read_source_if_signed_in(
    kind: &SourceKind,
    path: &str,
) -> PluginResult<Option<Credential>> {
    nora_provider_plugin_sdk::require_subscription_accounts()?;
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err(expired());
    }
    match std::fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(metadata) if metadata.is_file() => {}
        _ => return Err(expired()),
    }
    match kind {
        SourceKind::Cli => {
            let db = rusqlite::Connection::open_with_flags(
                path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|_| expired())?;
            db.busy_timeout(std::time::Duration::from_secs(3))
                .map_err(|_| expired())?;
            cli_snapshot(&db).map(|snapshot| snapshot.map(|(credential, _, _)| credential))
        }
        SourceKind::Ide => {
            let value = read_json(path)?;
            ide_credential(&value, path)
        }
        SourceKind::Magpie(account) => crate::magpie::read_credential(account, path),
    }
}
pub(crate) fn ide_credential(value: &Value, path: &Path) -> PluginResult<Option<Credential>> {
    if !value.is_object() {
        return Err(expired());
    }
    match value.get("accessToken") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(token)) if token.is_empty() => return Ok(None),
        _ => {}
    }
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
        realm: realm_value(value.get("startUrl").or_else(|| value.get("issuerUrl")))?,
        ..Default::default()
    };
    let social = value["authMethod"]
        .as_str()
        .is_some_and(|method| method.eq_ignore_ascii_case("social"));
    if !social {
        if let Some(hash) = text(value.get("clientIdHash"), 128) {
            if !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(expired());
            }
            if let Ok(registration) = read_json(
                &path
                    .parent()
                    .ok_or_else(expired)?
                    .join(format!("{hash}.json")),
            ) {
                c.client_id = text(registration.get("clientId"), 4096)
                    .unwrap_or_default()
                    .into();
                c.client_secret = text(registration.get("clientSecret"), 16 * 1024)
                    .unwrap_or_default()
                    .into();
            }
        }
    }
    let explicit_idc = value["authMethod"]
        .as_str()
        .is_some_and(|method| method.eq_ignore_ascii_case("idc"))
        || value["provider"].as_str().is_some_and(|provider| {
            ["builderid", "enterprise", "internal"]
                .iter()
                .any(|method| provider.eq_ignore_ascii_case(method))
        });
    if social || c.client_id.is_empty() && !explicit_idc {
        c.method = "social".into();
    }
    if c.builder {
        c.realm.clear();
    }
    Ok(Some(c))
}
pub(crate) const CLI_KEYS: [(&str, &str); 3] = [
    ("social", "kirocli:social:token"),
    ("idc", "kirocli:odic:token"),
    ("external-idp", "kirocli:external-idp:token"),
];
pub(crate) fn cli_snapshot(
    db: &rusqlite::Connection,
) -> PluginResult<Option<(Credential, String, String)>> {
    use rusqlite::OptionalExtension;
    let read = |key: &str| -> PluginResult<Option<(String, Value)>> {
        let raw: Option<String> = db
            .query_row("SELECT value FROM auth_kv WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|_| {
                PluginError::new(
                    "kiro_source_unreadable",
                    "The Kiro source database could not be read.",
                )
            })?;
        let Some(raw) = raw else { return Ok(None) };
        if raw.len() > 1024 * 1024 {
            return Err(expired());
        }
        let value: Value = serde_json::from_str(&raw).map_err(|_| expired())?;
        if !value.is_object() {
            return Err(expired());
        }
        Ok(Some((raw, value)))
    };
    let mut unreadable = false;
    for (method, key) in CLI_KEYS {
        let (raw, value) = match read(key) {
            Ok(Some(value)) => value,
            Ok(None) => continue,
            Err(error) if error.code == "kiro_source_unreadable" => return Err(error),
            Err(_) => {
                unreadable = true;
                continue;
            }
        };
        match value.get("access_token") {
            None | Some(Value::Null) => continue,
            Some(Value::String(token)) if token.is_empty() => continue,
            _ => {}
        }
        let mut credential = Credential {
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
            credential.realm = realm_value(value.get("start_url"))?;
            credential.builder = credential.realm.is_empty();
            if let Ok(Some((_, registration))) = read("kirocli:odic:device-registration") {
                credential.client_id = text(registration.get("client_id"), 4096)
                    .unwrap_or_default()
                    .into();
                credential.client_secret = text(registration.get("client_secret"), 16 * 1024)
                    .unwrap_or_default()
                    .into();
            }
        }
        if method == "external-idp" {
            credential.client_id = text(value.get("client_id"), 4096)
                .unwrap_or_default()
                .into();
            credential.realm = realm_value(value.get("issuer_url"))?;
            credential.token_endpoint = match value.get("token_endpoint") {
                None | Some(Value::Null) => {
                    if credential.realm.is_empty() {
                        String::new()
                    } else {
                        token_endpoint(&format!("{}/v1/token", credential.realm))?
                    }
                }
                Some(Value::String(value)) if value.is_empty() => {
                    if credential.realm.is_empty() {
                        String::new()
                    } else {
                        token_endpoint(&format!("{}/v1/token", credential.realm))?
                    }
                }
                Some(Value::String(value)) => token_endpoint(value)?,
                _ => return Err(expired()),
            };
            if credential.realm.is_empty() && !credential.token_endpoint.is_empty() {
                credential.realm = external_realm(&credential.token_endpoint)?;
            }
        }
        return Ok(Some((credential, key.into(), raw)));
    }
    if unreadable {
        Err(expired())
    } else {
        Ok(None)
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
