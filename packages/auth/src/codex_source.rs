//! Codex's own ChatGPT sign-in, used where Codex keeps it: `auth.json` in
//! `$CODEX_HOME`, or in `~/.codex`. Nora saves only which file and whose
//! account it is. Every request reads the tokens again, and one about to
//! expire is renewed and written back into Codex's file, so Codex and Nora go
//! on sharing one refresh token instead of rotating it away from each other.

use crate::grok_source::METADATA_KEY;
use crate::oauth_providers::OpenAICodexOAuthProvider;
use nora_ai::http::HttpClient;
use nora_ai::json_value::JsonValue;
use nora_ai::oauth::{now_millis, OAuthCredentials, OAuthProviderId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

/// The sign-in method and source kind of a Codex account.
pub const KIND: &str = "codexCLI";
const ACCOUNT_KEY: &str = "noraCodexAccountID";
const SUBJECT_KEY: &str = "noraCodexAccountSubject";
const MAX_AUTH_BYTES: u64 = 1_048_576;
const RENEW_BEFORE_MILLIS: i64 = 5 * 60 * 1000;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct Source {
    kind: String,
    auth_file_path: PathBuf,
}

#[derive(Debug, Eq, PartialEq)]
pub enum SourceError {
    InvalidSource,
    NotSignedIn,
    InvalidCredentials,
    AccountChanged,
    RefreshRefused,
    RefreshFailed,
}

impl SourceError {
    pub fn message(&self) -> &'static str {
        match self {
            Self::InvalidSource => "Reconnect the Codex account in Nora.",
            Self::NotSignedIn => {
                "Codex is not signed in with ChatGPT. Sign in to Codex and try again."
            }
            Self::InvalidCredentials => {
                "Codex's sign-in file could not be read. Sign in to Codex again."
            }
            Self::AccountChanged => {
                "Codex is signed in to a different account. Reconnect the account in Nora."
            }
            Self::RefreshRefused => "Codex's sign-in was refused. Sign in to Codex again.",
            Self::RefreshFailed => "Codex's sign-in could not be renewed. Try again.",
        }
    }
}

/// Codex's sign-in file for this user.
pub fn auth_file(home: &Path, environment: &BTreeMap<String, String>) -> PathBuf {
    environment
        .get("CODEX_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".codex"))
        .join("auth.json")
}

pub fn is_bound(credentials: &OAuthCredentials) -> bool {
    credentials.provider == OAuthProviderId::OpenAICodex
        && credentials
            .extras
            .get(METADATA_KEY)
            .and_then(|value| {
                value
                    .as_value()
                    .get("kind")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .as_deref()
            == Some(KIND)
}

/// The account Codex is signed in to, as a reference Nora can save: which
/// file, whose account, and what to show for it. It holds no token.
pub fn bind(auth_file_path: &Path) -> Result<OAuthCredentials, SourceError> {
    nora_provider_plugin_sdk::require_subscription_accounts()
        .map_err(|_| SourceError::InvalidSource)?;
    let tokens = read(auth_file_path)?;
    let mut extras =
        OpenAICodexOAuthProvider::profile_extras_from_tokens(&tokens.access, tokens.id.as_deref());
    let source = Source {
        kind: KIND.into(),
        auth_file_path: auth_file_path.to_path_buf(),
    };
    extras.insert(
        METADATA_KEY.into(),
        JsonValue::from(serde_json::to_value(source).map_err(|_| SourceError::InvalidSource)?),
    );
    extras.insert(ACCOUNT_KEY.into(), JsonValue::string(&tokens.account));
    extras.insert("accountId".into(), JsonValue::string(&tokens.account));
    if let Some(subject) = &tokens.subject {
        extras.insert(SUBJECT_KEY.into(), JsonValue::string(subject));
        extras.insert(
            "accountIdentity".into(),
            serde_json::json!({
                "issuer":"https://auth.openai.com", "subject":subject, "scope":tokens.account
            })
            .into(),
        );
    }
    Ok(OAuthCredentials::new(
        OAuthProviderId::OpenAICodex,
        "",
        None,
        None,
        extras,
    ))
}

/// The bound account with a current access token, renewed in Codex's file
/// first when it is about to expire or `force` asks for it.
pub async fn resolve(
    credentials: &OAuthCredentials,
    force: bool,
    client: &dyn HttpClient,
) -> Result<OAuthCredentials, SourceError> {
    nora_provider_plugin_sdk::require_subscription_accounts()
        .map_err(|_| SourceError::InvalidSource)?;
    let source: Source = credentials
        .extras
        .get(METADATA_KEY)
        .and_then(|value| serde_json::from_value(value.as_value().clone()).ok())
        .filter(|source: &Source| source.kind == KIND && source.auth_file_path.is_absolute())
        .ok_or(SourceError::InvalidSource)?;
    let account = credentials
        .extras
        .get(ACCOUNT_KEY)
        .and_then(|value| value.as_str())
        .filter(|account| !account.is_empty())
        .ok_or(SourceError::InvalidSource)?
        .to_string();
    let subject = credentials
        .extras
        .get(SUBJECT_KEY)
        .and_then(|value| value.as_str())
        .or_else(|| {
            credentials
                .extras
                .get("accountIdentity")
                .and_then(|identity| identity.as_value().get("subject"))
                .and_then(Value::as_str)
        });
    let observed = read(&source.auth_file_path)?;
    ensure_account(&observed, &account, subject)?;
    let lock = file_lock(&source.auth_file_path);
    let _guard = lock.lock().await;
    // Codex may have renewed the tokens while this request waited.
    let mut tokens = read(&source.auth_file_path)?;
    ensure_account(&tokens, &account, subject)?;
    let renew = (force && tokens.access == observed.access)
        || tokens
            .expires_at
            .is_none_or(|expires| expires <= now_millis() + RENEW_BEFORE_MILLIS);
    if renew {
        let refresh = tokens.refresh.as_deref().ok_or(SourceError::NotSignedIn)?;
        let renewed = OpenAICodexOAuthProvider::default()
            .refresh_tokens(refresh, client)
            .await
            .map_err(|error| {
                if error.requires_sign_in() {
                    SourceError::RefreshRefused
                } else {
                    SourceError::RefreshFailed
                }
            })?;
        write_back(&source.auth_file_path, &renewed, &account, subject)?;
        tokens = read(&source.auth_file_path)?;
        ensure_account(&tokens, &account, subject)?;
    }
    let mut transient = credentials.clone();
    transient.access_token = tokens.access;
    transient.refresh_token = None;
    transient.expires_at = tokens.expires_at;
    Ok(transient)
}

struct Tokens {
    access: String,
    refresh: Option<String>,
    id: Option<String>,
    account: String,
    subject: Option<String>,
    expires_at: Option<i64>,
}

fn ensure_account(
    tokens: &Tokens,
    account: &str,
    subject: Option<&str>,
) -> Result<(), SourceError> {
    if tokens.account != account
        || subject.is_some_and(|expected| tokens.subject.as_deref() != Some(expected))
    {
        return Err(SourceError::AccountChanged);
    }
    Ok(())
}

fn read(path: &Path) -> Result<Tokens, SourceError> {
    tokens_from_file(&read_file(path)?)
}

fn tokens_from_file(value: &Value) -> Result<Tokens, SourceError> {
    if value.get("auth_mode").and_then(Value::as_str) == Some("apikey") {
        return Err(SourceError::NotSignedIn);
    }
    let tokens = value.get("tokens").ok_or(SourceError::NotSignedIn)?;
    let text = |key: &str| {
        tokens
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let access = text("access_token").ok_or(SourceError::NotSignedIn)?;
    let account = text("account_id")
        .or_else(|| OpenAICodexOAuthProvider::extract_account_id_from_jwt(&access))
        .ok_or(SourceError::InvalidCredentials)?;
    if OpenAICodexOAuthProvider::extract_account_id_from_jwt(&access)
        .is_some_and(|claim| claim != account)
    {
        return Err(SourceError::AccountChanged);
    }
    let subject = OpenAICodexOAuthProvider::profile_extras_from_tokens(&access, None)
        .get("accountIdentity")
        .and_then(|identity| identity.as_value().get("subject"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            let id = text("id_token")?;
            OpenAICodexOAuthProvider::profile_extras_from_tokens(&id, None)
                .get("accountIdentity")
                .and_then(|identity| identity.as_value().get("subject"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    Ok(Tokens {
        expires_at: jwt_expiry(&access),
        subject,
        access,
        refresh: text("refresh_token"),
        id: text("id_token"),
        account,
    })
}

fn read_file(path: &Path) -> Result<Value, SourceError> {
    let file = std::fs::File::open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            SourceError::NotSignedIn
        } else {
            SourceError::InvalidCredentials
        }
    })?;
    let mut data = Vec::new();
    file.take(MAX_AUTH_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|_| SourceError::InvalidCredentials)?;
    if data.len() as u64 > MAX_AUTH_BYTES {
        return Err(SourceError::InvalidCredentials);
    }
    serde_json::from_slice(&data).map_err(|_| SourceError::InvalidCredentials)
}

/// Puts renewed tokens into Codex's file the way Codex writes them, keeping
/// every other field, and replaces the file in one step.
fn write_back(
    path: &Path,
    renewed: &nora_ai::oauth::OAuthTokenResponse,
    expected_account: &str,
    expected_subject: Option<&str>,
) -> Result<(), SourceError> {
    let mut value = read_file(path)?;
    ensure_account(
        &tokens_from_file(&value)?,
        expected_account,
        expected_subject,
    )?;
    let tokens = value
        .get_mut("tokens")
        .and_then(Value::as_object_mut)
        .ok_or(SourceError::InvalidCredentials)?;
    tokens.insert(
        "access_token".into(),
        Value::String(renewed.access_token.clone()),
    );
    if let Some(refresh) = &renewed.refresh_token {
        tokens.insert("refresh_token".into(), Value::String(refresh.clone()));
    }
    if let Some(id) = &renewed.id_token {
        tokens.insert("id_token".into(), Value::String(id.clone()));
    }
    ensure_account(
        &tokens_from_file(&value)?,
        expected_account,
        expected_subject,
    )?;
    value["last_refresh"] = Value::String(chrono::Utc::now().to_rfc3339());
    let bytes = serde_json::to_vec_pretty(&value).map_err(|_| SourceError::InvalidCredentials)?;
    let directory = path.parent().ok_or(SourceError::InvalidSource)?;
    let staged = directory.join(format!(".auth.json.nora-{}", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode_private()
            .open(&staged)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&staged, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result.map_err(|_| SourceError::RefreshFailed)
}

trait PrivateMode {
    fn mode_private(&mut self) -> &mut Self;
}

impl PrivateMode for std::fs::OpenOptions {
    #[cfg(unix)]
    fn mode_private(&mut self) -> &mut Self {
        std::os::unix::fs::OpenOptionsExt::mode(self, 0o600)
    }
    #[cfg(not(unix))]
    fn mode_private(&mut self) -> &mut Self {
        self
    }
}

fn jwt_expiry(token: &str) -> Option<i64> {
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let claims: Value = serde_json::from_slice(&decoded).ok()?;
    claims
        .get("exp")?
        .as_i64()
        .map(|seconds| seconds.saturating_mul(1000))
}

fn file_lock(path: &Path) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: LazyLock<Mutex<BTreeMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> =
        LazyLock::new(Default::default);
    LOCKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entry(path.to_path_buf())
        .or_default()
        .clone()
}

#[cfg(test)]
#[path = "../test/codex_source.test.rs"]
mod tests;
