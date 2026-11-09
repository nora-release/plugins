use provider_support::{PluginError, PluginResult};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
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
    pub login_provider: String,
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
pub(crate) fn login_provider(value: Option<&Value>) -> String {
    match text(value, 128)
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "google" => "Google".into(),
        "github" => "Github".into(),
        "builderid" => "BuilderId".into(),
        "enterprise" | "awsidc" => "Enterprise".into(),
        "internal" => "Internal".into(),
        other => other.into(),
    }
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
            login_provider: login_provider(value.get("loginProvider")),
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
            if Some(previous) != identity.as_ref()
                && !credential.is_legacy_provider_identity(previous)
            {
                return Err(expired());
            }
        }
        if matches!((value.get("accountIdentity"), value.pointer("/extras/accountIdentity")), (Some(a), Some(b)) if a != b)
        {
            return Err(expired());
        }
        Ok(credential)
    }
    pub fn saved(&self) -> Value {
        let mut value = json!({"type":if self.method=="apikey"{"api"}else{"oauth"},"authMethod":if self.method=="apikey"{"apiKey"}else{"oauth"},"method":self.method,"loginProvider":self.login_provider,"access":self.access,"refresh":self.refresh,"expires":if self.method=="apikey"{Value::Null}else{json!(self.expires)},"region":self.region,"profileArn":self.profile,"clientId":self.client_id,"clientSecret":self.client_secret,"tokenEndpoint":self.token_endpoint,"builderID":self.builder,"accountId":self.identity,"email":self.email,"userId":self.user_id,"realm":self.realm,"plan":self.plan,"label":self.label()});
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
        let mut identity = json!({"issuer":"https://kiro.dev","subject":subject,"scope":scope});
        if self.user_id.is_empty() && !self.login_provider.is_empty() {
            identity["discriminator"] = json!(format!(
                "login-provider:{}",
                self.login_provider.to_ascii_lowercase()
            ));
        }
        Some(identity)
    }
    fn is_legacy_provider_identity(&self, previous: &Value) -> bool {
        if self.login_provider.is_empty() || previous.get("discriminator").is_some() {
            return false;
        }
        let mut original = self.clone();
        original.login_provider.clear();
        original.account_identity().as_ref() == Some(previous)
    }
    pub fn label(&self) -> String {
        if self.email.is_empty() {
            return "Kiro account".into();
        }
        let provider = match self.login_provider.as_str() {
            "Google" => "Google",
            "Github" => "GitHub",
            "BuilderId" => "Builder ID",
            "Enterprise" => "IAM Identity Center",
            "Internal" => "Internal",
            _ => return self.email.clone(),
        };
        format!("{} ({provider})", self.email)
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
        let previous_provider = login_provider(saved.get("loginProvider"));
        let provider_matches =
            previous_provider.is_empty() || previous_provider == self.login_provider;
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
            if !provider_matches {
                return false;
            }
            if previous_provider == self.login_provider
                && self.is_legacy_provider_identity(previous)
            {
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
        if !provider_matches
            || self.email.is_empty()
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
#[cfg(test)]
#[path = "../test/session.test.rs"]
mod tests;
