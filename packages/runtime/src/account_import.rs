//! Private account migration contracts. These values cross only the plugin/host
//! channel; the app receives the host's separately whitelisted batch results.
mod login;
mod parse;

use async_trait::async_trait;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const MAX_ENTRIES: usize = 200;
pub const MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoginKind {
    Codex,
    Claude,
}
impl LoginKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::Codex => "openai",
            Self::Claude => "anthropic",
        }
    }
    fn formats(self) -> &'static [&'static str] {
        match self {
            Self::Codex => &[
                "auto",
                "codex-auth",
                "cli-proxy",
                "cockpit",
                "sub2api",
                "codexbar",
                "refresh-token",
                "nora-account",
                "magpie-login",
            ],
            Self::Claude => &[
                "auto",
                "claude-credentials",
                "cli-proxy",
                "sub2api",
                "refresh-token",
                "nora-account",
                "magpie-login",
            ],
        }
    }
}

#[async_trait]
pub trait CredentialPersistence: Send + Sync {
    async fn save(&self, credentials: Value) -> PluginResult<()>;
}
pub struct HostPersistence;
#[async_trait]
impl CredentialPersistence for HostPersistence {
    async fn save(&self, credentials: Value) -> PluginResult<()> {
        nora_provider_plugin_sdk::save_credentials(credentials).await
    }
}

pub fn fingerprint(provider: &str, refresh: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("{provider}\0{refresh}").as_bytes())
    )
}
pub fn declaration(kind: LoginKind) -> Value {
    json!({"formats":kind.formats(),"refreshesTokens":kind==LoginKind::Codex,"maxEntries":MAX_ENTRIES,"maxBytes":MAX_BYTES})
}

pub fn inspect(kind: LoginKind, params: &Value) -> PluginResult<Value> {
    nora_provider_plugin_sdk::require_subscription_accounts()?;
    let input = ImportInput::read(params)?;
    let entries = parse::parse(kind, &input)?;
    let mut seen = std::collections::BTreeMap::new();
    let secrets = entries
        .iter()
        .flat_map(parse::Entry::secrets)
        .collect::<Vec<_>>();
    let mut public = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let mut label = entry.email.clone();
        if label.is_empty() {
            label = format!("#{}", index + 1);
        }
        label = redact(label, &secrets);
        let mut item = json!({"index":index,"label":label});
        if let Some(error) = &entry.error {
            item["error"] = json!(error);
        } else {
            let fingerprint = fingerprint(kind.id(), &entry.refresh);
            if let Some(previous) = seen.get(&fingerprint) {
                item["duplicateOf"] = json!(previous);
            } else {
                seen.insert(fingerprint.clone(), index);
            }
            item["fingerprint"] = json!(fingerprint);
        }
        public.push(item);
    }
    Ok(json!({"entries":public}))
}

pub async fn import_login(
    kind: LoginKind,
    params: &Value,
    client: &dyn nora_ai::HttpClient,
    persistence: &dyn CredentialPersistence,
) -> PluginResult<Value> {
    nora_provider_plugin_sdk::require_subscription_accounts()?;
    let input = ImportInput::read(params)?;
    let entries = parse::parse(kind, &input)?;
    let index = params
        .get("index")
        .map(|value| {
            value
                .as_u64()
                .and_then(|index| usize::try_from(index).ok())
                .ok_or_else(|| invalid("Choose a valid account entry."))
        })
        .transpose()?
        .unwrap_or(0);
    let entry = entries
        .get(index)
        .ok_or_else(|| invalid("Choose a valid account entry."))?;
    if let Some(error) = &entry.error {
        return Err(invalid(error));
    }
    let fingerprint = fingerprint(kind.id(), &entry.refresh);
    if params
        .get("fingerprint")
        .is_some_and(|value| value.as_str() != Some(&fingerprint))
    {
        return Err(invalid(
            "The selected import entry changed. Inspect the file again.",
        ));
    }
    let resumed = resumed(params, &fingerprint)?;
    login::import(kind, entry, resumed, &fingerprint, client, persistence).await
}

pub struct ImportInput<'a> {
    pub file_name: &'a str,
    pub content: &'a str,
    pub format: &'a str,
}
impl<'a> ImportInput<'a> {
    pub fn read(params: &'a Value) -> PluginResult<Self> {
        let content = params
            .get("content")
            .and_then(Value::as_str)
            .filter(|content| !content.trim().is_empty() && content.len() <= MAX_BYTES)
            .ok_or_else(|| invalid("Choose nonempty account content smaller than 4 MiB."))?;
        let format = params
            .get("format")
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| invalid("The account import format is invalid."))
            })
            .transpose()?
            .unwrap_or("auto");
        let file_name = params
            .get("fileName")
            .map(|value| {
                value
                    .as_str()
                    .filter(|name| name.len() <= 1024 && !name.chars().any(char::is_control))
                    .ok_or_else(|| invalid("The account import filename is invalid."))
            })
            .transpose()?
            .unwrap_or("");
        Ok(Self {
            file_name,
            content,
            format,
        })
    }
}

/// Only the host's private staging record can resume a rotated token. It must
/// still name the original entry, even though its refresh token has changed.
pub fn resumed(params: &Value, expected: &str) -> PluginResult<Option<Value>> {
    let Some(credentials) = params.get("credentials").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    if credentials.get("type").and_then(Value::as_str) == Some("pendingImport") {
        return Ok(None);
    }
    if !credentials.is_object()
        || credentials
            .pointer("/extras/noraImportFingerprint")
            .and_then(Value::as_str)
            != Some(expected)
        || credentials.get("source").is_some()
        || credentials.pointer("/extras/noraAccountSource").is_some()
    {
        return Err(invalid(
            "The saved import does not match this account entry.",
        ));
    }
    Ok(Some(credentials.clone()))
}
pub fn mark(credentials: &mut Value, fingerprint: &str, state: &str) {
    if !credentials.get("extras").is_some_and(Value::is_object) {
        credentials["extras"] = json!({});
    }
    credentials["extras"]["noraImportFingerprint"] = json!(fingerprint);
    credentials["extras"]["noraImportState"] = json!(state);
}
pub fn redact(mut text: String, secrets: &[String]) -> String {
    let mut secrets = secrets
        .iter()
        .filter(|secret| !secret.is_empty())
        .collect::<Vec<_>>();
    secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    for secret in secrets {
        text = text.replace(secret, "[redacted]");
    }
    text
}
pub fn result(credentials: Value, refreshed: bool) -> PluginResult<Value> {
    let identity = credentials
        .get("accountIdentity")
        .or_else(|| credentials.pointer("/extras/accountIdentity"))
        .filter(|identity| identity.is_object())
        .ok_or_else(|| invalid("The account did not provide a stable identity."))?
        .clone();
    let secrets = [
        credentials.get("access"),
        credentials.get("refresh"),
        credentials.pointer("/extras/idToken"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .map(str::to_string)
    .collect::<Vec<_>>();
    let mut profile = json!({});
    for key in ["email", "name", "plan"] {
        if let Some(value) = credentials
            .get(key)
            .or_else(|| credentials.get("extras").and_then(|extras| extras.get(key)))
            .and_then(Value::as_str)
            .filter(|value| value.len() <= 512 && !value.chars().any(char::is_control))
        {
            profile[key] = json!(redact(value.to_string(), &secrets));
        }
    }
    Ok(
        json!({"credentials":credentials,"accountIdentity":identity,"profile":profile,"refreshed":refreshed}),
    )
}
pub async fn checkpoint(
    persistence: &dyn CredentialPersistence,
    credentials: &Value,
) -> PluginResult<()> {
    login::checkpoint(persistence, credentials).await
}
/// Private import bookkeeping survives normal token renewal. Newly identified
/// provider identity always takes precedence over a restored import identity.
pub fn preserve_metadata(previous: &Value, next: &mut Value) {
    for key in ["noraImportFingerprint", "noraImportState"] {
        if let Some(value) = previous
            .get("extras")
            .and_then(|extras| extras.get(key))
            .filter(|value| value.is_string())
        {
            if !next.get("extras").is_some_and(Value::is_object) {
                next["extras"] = json!({});
            }
            next["extras"][key] = value.clone();
        }
    }
    if !next
        .get("accountIdentity")
        .or_else(|| next.pointer("/extras/accountIdentity"))
        .is_some_and(Value::is_object)
    {
        if let Some(identity) = previous
            .get("accountIdentity")
            .or_else(|| previous.pointer("/extras/accountIdentity"))
            .filter(|value| value.is_object())
        {
            next["accountIdentity"] = identity.clone();
        }
    }
}
pub fn invalid(message: &str) -> PluginError {
    PluginError::new("invalid_account_import", message)
}

#[cfg(test)]
#[path = "../test/account_import.test.rs"]
mod tests;
