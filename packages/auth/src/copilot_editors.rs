//! Copilot's local sign-ins remain in the editors' and CLI's own stores.
//! Nora keeps an account reference and verifies whose token it reads each time.

use crate::{copilot_identity, grok_source::METADATA_KEY, GitHubCopilotOAuthProvider};
use nora_ai::{
    HttpClient, HttpMethod, HttpRequest, JsonValue, OAuthCredentials, OAuthProvider,
    OAuthProviderId,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const KIND: &str = "copilotEditors";
const MAX_AUTH_BYTES: u64 = 1_048_576;
const MAX_ACCOUNTS: usize = 64;

#[derive(Debug, Eq, PartialEq)]
pub enum EditorsError {
    NotSignedIn,
    InvalidFile,
    Rejected,
    AccountChanged,
    InvalidSource,
}

impl EditorsError {
    pub fn message(&self) -> &'static str {
        match self {
            Self::NotSignedIn => "Copilot is not signed in on this Mac. Sign in to Copilot and try again.",
            Self::InvalidFile => "Copilot's local sign-in file could not be read.",
            Self::Rejected => "GitHub did not accept Copilot's local sign-in. Sign in to Copilot again.",
            Self::AccountChanged => "Copilot is using a different account. The previous account's sign-in is unavailable.",
            Self::InvalidSource => "Reconnect this Copilot account in Nora.",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Source {
    kind: String,
    auth_file_path: PathBuf,
    origin: Origin,
    host: String,
    login: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fallback_cli_file_path: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum Origin {
    Editor,
    Cli,
}

struct Candidate {
    source: Source,
    token: String,
}

struct CachedSession {
    path: PathBuf,
    host: String,
    origin: Origin,
    token: String,
    credentials: OAuthCredentials,
    until: Instant,
}

fn sessions() -> &'static Mutex<Vec<CachedSession>> {
    static SESSIONS: OnceLock<Mutex<Vec<CachedSession>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn apps_file(home: &Path, environment: &BTreeMap<String, String>) -> PathBuf {
    environment
        .get("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
        .join("github-copilot")
        .join("apps.json")
}

pub fn cli_file(home: &Path, environment: &BTreeMap<String, String>) -> PathBuf {
    environment
        .get("COPILOT_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".copilot"))
        .join("config.json")
}

pub fn is_bound(credentials: &OAuthCredentials) -> bool {
    credentials.provider == OAuthProviderId::GitHubCopilot
        && credentials
            .extras
            .get(METADATA_KEY)
            .and_then(|source| source.as_value().get("kind").and_then(Value::as_str))
            == Some(KIND)
}

/// An explicit connection uses the same continuously read reference as discovery.
pub async fn take_over(
    path: &Path,
    client: &dyn HttpClient,
) -> Result<OAuthCredentials, EditorsError> {
    let candidates = read_editors(path)?;
    let candidate = candidates
        .into_iter()
        .next()
        .ok_or(EditorsError::NotSignedIn)?;
    bind(candidate, client).await
}

/// Empty means signed out. Invalid files and failed verification are errors so
/// discovery cannot mistake a temporary failure for removal of an account.
pub async fn discover(
    home: &Path,
    environment: &BTreeMap<String, String>,
    client: &dyn HttpClient,
) -> Result<Vec<OAuthCredentials>, EditorsError> {
    let apps = apps_file(home, environment);
    let mut candidates = read_editors(&apps)?;
    candidates.extend(read_editors(&apps.with_file_name("hosts.json"))?);
    let cli = cli_file(home, environment);
    for candidate in &mut candidates {
        candidate.source.fallback_cli_file_path = Some(cli.clone());
    }
    candidates.extend(read_cli(&cli).await?);
    if candidates.len() > MAX_ACCOUNTS {
        return Err(EditorsError::InvalidFile);
    }
    let mut accounts = Vec::new();
    let mut seen = BTreeSet::new();
    // The CLI can replace a refused editor token for the same account.
    let mut failures = BTreeMap::new();
    for candidate in candidates {
        let key = (
            candidate.source.host.clone(),
            candidate.source.login.to_ascii_lowercase(),
        );
        if !key.1.is_empty() && seen.contains(&key) {
            continue;
        }
        match bind(candidate, client).await {
            Ok(account) => {
                let key = (
                    text(&account, "githubHost").unwrap_or("").to_owned(),
                    text(&account, "githubLogin")
                        .unwrap_or("")
                        .to_ascii_lowercase(),
                );
                failures.remove(&key);
                if seen.insert(key) {
                    accounts.push(account);
                }
            }
            Err(error) => {
                failures.insert(key, error);
            }
        }
    }
    if let Some((_, error)) = failures.into_iter().next() {
        return Err(error);
    }
    Ok(accounts)
}

pub async fn resolve(
    credentials: &OAuthCredentials,
    client: &dyn HttpClient,
) -> Result<OAuthCredentials, EditorsError> {
    resolve_with_force(credentials, false, client).await
}

pub async fn resolve_with_force(
    credentials: &OAuthCredentials,
    force: bool,
    client: &dyn HttpClient,
) -> Result<OAuthCredentials, EditorsError> {
    let source: Source = credentials
        .extras
        .get(METADATA_KEY)
        .and_then(|value| serde_json::from_value(value.as_value().clone()).ok())
        .filter(|source: &Source| {
            source.kind == KIND
                && source.auth_file_path.is_absolute()
                && source
                    .fallback_cli_file_path
                    .as_ref()
                    .is_none_or(|path| path.is_absolute())
                && copilot_identity::normalize_host(&source.host).as_deref()
                    == Some(source.host.as_str())
        })
        .ok_or(EditorsError::InvalidSource)?;
    let expected = credentials
        .extras
        .get(copilot_identity::IDENTITY_KEY)
        .ok_or(EditorsError::InvalidSource)?;
    if force {
        sessions()
            .lock()
            .map_err(|_| EditorsError::Rejected)?
            .retain(|session| {
                session.path != source.auth_file_path
                    && source.fallback_cli_file_path.as_ref() != Some(&session.path)
            });
    }
    let mut candidates = match source.origin {
        Origin::Editor => read_editors(&source.auth_file_path)?,
        Origin::Cli => read_cli(&source.auth_file_path).await?,
    };
    candidates.retain(|candidate| candidate.source.host == source.host);
    candidates.sort_by_key(|candidate| !candidate.source.login.eq_ignore_ascii_case(&source.login));
    let result = resolve_candidates(candidates, expected, client).await;
    if result.is_ok() || source.origin != Origin::Editor {
        return result;
    }
    if let Some(path) = &source.fallback_cli_file_path {
        let mut fallback = read_cli(path).await?;
        fallback.retain(|candidate| candidate.source.host == source.host);
        fallback
            .sort_by_key(|candidate| !candidate.source.login.eq_ignore_ascii_case(&source.login));
        if let Ok(current) = resolve_candidates(fallback, expected, client).await {
            return Ok(current);
        }
    }
    result
}

async fn resolve_candidates(
    candidates: Vec<Candidate>,
    expected: &JsonValue,
    client: &dyn HttpClient,
) -> Result<OAuthCredentials, EditorsError> {
    let mut error = EditorsError::NotSignedIn;
    for candidate in candidates {
        match transient(&candidate, client).await {
            Ok(current) if current.extras.get(copilot_identity::IDENTITY_KEY) == Some(expected) => {
                return Ok(current)
            }
            Ok(_) => error = EditorsError::AccountChanged,
            Err(failure) => error = failure,
        }
    }
    Err(error)
}

async fn bind(
    candidate: Candidate,
    client: &dyn HttpClient,
) -> Result<OAuthCredentials, EditorsError> {
    let mut credentials = transient(&candidate, client).await?;
    credentials.extras.insert(
        METADATA_KEY.into(),
        JsonValue::from(
            serde_json::to_value(&candidate.source).map_err(|_| EditorsError::InvalidSource)?,
        ),
    );
    credentials.access_token.clear();
    credentials.refresh_token = None;
    credentials.expires_at = None;
    Ok(credentials)
}

async fn transient(
    candidate: &Candidate,
    client: &dyn HttpClient,
) -> Result<OAuthCredentials, EditorsError> {
    if let Some(credentials) = sessions()
        .lock()
        .map_err(|_| EditorsError::Rejected)?
        .iter()
        .find(|session| {
            session.path == candidate.source.auth_file_path
                && session.host == candidate.source.host
                && session.origin == candidate.source.origin
                && session.token == candidate.token
                && session.until > Instant::now()
        })
        .map(|session| session.credentials.clone())
    {
        return Ok(credentials);
    }
    let credentials = fetch_session(candidate, client).await?;
    let remaining = credentials
        .expires_at
        .map(|expires| {
            expires
                .saturating_sub(nora_ai::oauth::now_millis())
                .saturating_sub(120_000)
                .max(0) as u64
        })
        .unwrap_or(0)
        .min(25 * 60 * 1000);
    if remaining > 0 {
        let mut sessions = sessions().lock().map_err(|_| EditorsError::Rejected)?;
        sessions.retain(|session| session.until > Instant::now());
        if sessions.len() >= MAX_ACCOUNTS {
            sessions.remove(0);
        }
        sessions.push(CachedSession {
            path: candidate.source.auth_file_path.clone(),
            host: candidate.source.host.clone(),
            origin: candidate.source.origin,
            token: candidate.token.clone(),
            credentials: credentials.clone(),
            until: Instant::now() + Duration::from_millis(remaining),
        });
    }
    Ok(credentials)
}

async fn fetch_session(
    candidate: &Candidate,
    client: &dyn HttpClient,
) -> Result<OAuthCredentials, EditorsError> {
    let mut github = OAuthCredentials::new(
        OAuthProviderId::GitHubCopilot,
        "",
        Some(candidate.token.clone()),
        Some(0),
        BTreeMap::new(),
    );
    github.extras.insert(
        "githubHost".into(),
        JsonValue::string(&candidate.source.host),
    );
    copilot_identity::enrich(&mut github, client)
        .await
        .map_err(|_| EditorsError::Rejected)?;
    if candidate.source.origin == Origin::Editor {
        let provider = GitHubCopilotOAuthProvider::new(
            copilot_identity::api_url(&candidate.source.host, "/copilot_internal/v2/token"),
            GitHubCopilotOAuthProvider::default().extra_headers,
        );
        return provider
            .refresh(&github, client)
            .await
            .map_err(|_| EditorsError::Rejected);
    }
    let response = client
        .request(
            HttpRequest::new(
                HttpMethod::Get,
                copilot_identity::api_url(&candidate.source.host, "/copilot_internal/user"),
            )
            .with_header("authorization", format!("token {}", candidate.token))
            .with_header("accept", "application/json")
            .with_header("user-agent", "copilot/1.0.88"),
        )
        .await
        .map_err(|_| EditorsError::Rejected)?;
    if response.status != 200 || response.body.len() > MAX_AUTH_BYTES as usize {
        return Err(EditorsError::Rejected);
    }
    let payload: Value =
        serde_json::from_slice(&response.body).map_err(|_| EditorsError::Rejected)?;
    if let Some(endpoint) = payload.pointer("/endpoints/api").and_then(Value::as_str) {
        github
            .extras
            .insert("endpoint".into(), JsonValue::string(endpoint));
    } else if !candidate.source.host.is_empty() {
        github.extras.insert(
            "endpoint".into(),
            JsonValue::string(format!("https://copilot-api.{}", candidate.source.host)),
        );
    }
    if let Some(plan) = payload.get("copilot_plan").and_then(Value::as_str) {
        github.extras.insert("plan".into(), JsonValue::string(plan));
    }
    github
        .extras
        .insert("noraCopilotCLI".into(), JsonValue::from(Value::Bool(true)));
    github.access_token = candidate.token.clone();
    github.expires_at = Some(nora_ai::oauth::now_millis() + 30 * 60 * 1000);
    Ok(github)
}

fn read_editors(path: &Path) -> Result<Vec<Candidate>, EditorsError> {
    let Some(value) = read_file(path)? else {
        return Ok(Vec::new());
    };
    let entries = value.as_object().ok_or(EditorsError::InvalidFile)?;
    let mut candidates = Vec::new();
    for (key, app) in entries {
        if key.is_empty() {
            continue;
        }
        let Some(host) = copilot_identity::normalize_host(key.split(':').next().unwrap_or(""))
        else {
            continue;
        };
        let Some(token) = value_text(app, "oauth_token") else {
            continue;
        };
        candidates.push(Candidate {
            source: Source {
                kind: KIND.into(),
                auth_file_path: path.into(),
                origin: Origin::Editor,
                host,
                login: value_text(app, "user").unwrap_or("").into(),
                fallback_cli_file_path: None,
            },
            token: token.into(),
        });
        if candidates.len() > MAX_ACCOUNTS {
            return Err(EditorsError::InvalidFile);
        }
    }
    candidates.sort_by_key(|candidate| !candidate.source.host.is_empty());
    Ok(candidates)
}

async fn read_cli(path: &Path) -> Result<Vec<Candidate>, EditorsError> {
    let Some(value) = read_file(path)? else {
        return Ok(Vec::new());
    };
    if !value.is_object() {
        return Err(EditorsError::InvalidFile);
    }
    let mut users = Vec::new();
    if let Some(user) = value
        .get("lastLoggedInUser")
        .filter(|user| user.is_object())
    {
        users.push(user);
    }
    if let Some(others) = value.get("loggedInUsers").and_then(Value::as_array) {
        users.extend(others);
    }
    if users.len() > MAX_ACCOUNTS {
        return Err(EditorsError::InvalidFile);
    }
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    for user in users {
        let Some(login) = value_text(user, "login") else {
            continue;
        };
        let Some(host) = value_text(user, "host").and_then(copilot_identity::normalize_host) else {
            continue;
        };
        if !seen.insert((host.clone(), login.to_ascii_lowercase())) {
            continue;
        }
        let account = format!("{}:{login}", copilot_identity::issuer(&host));
        let token = value
            .get("copilotTokens")
            .and_then(|tokens| tokens.get(&account))
            .and_then(Value::as_str)
            .filter(|token| valid_text(token))
            .map(str::to_owned);
        let token = match token {
            Some(token) => Some(token),
            None => keychain_token(&account).await?,
        };
        let Some(token) = token else {
            continue;
        };
        candidates.push(Candidate {
            source: Source {
                kind: KIND.into(),
                auth_file_path: path.into(),
                origin: Origin::Cli,
                host,
                login: login.into(),
                fallback_cli_file_path: None,
            },
            token,
        });
    }
    candidates.sort_by_key(|candidate| !candidate.source.host.is_empty());
    Ok(candidates)
}

#[cfg(target_os = "macos")]
async fn keychain_token(account: &str) -> Result<Option<String>, EditorsError> {
    use std::{process::Stdio, time::Duration};
    let mut command = tokio::process::Command::new("/usr/bin/security");
    command
        .args([
            "find-generic-password",
            "-s",
            "copilot-cli",
            "-a",
            account,
            "-w",
        ])
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(5), command.output())
        .await
        .map_err(|_| EditorsError::Rejected)?
        .map_err(|_| EditorsError::Rejected)?;
    if output.status.code() == Some(44) {
        return Ok(None);
    }
    if !output.status.success() {
        return Err(EditorsError::Rejected);
    }
    let token = String::from_utf8(output.stdout).map_err(|_| EditorsError::InvalidFile)?;
    Ok(valid_text(token.trim()).then(|| token.trim().to_owned()))
}

#[cfg(not(target_os = "macos"))]
async fn keychain_token(_account: &str) -> Result<Option<String>, EditorsError> {
    Ok(None)
}

fn read_file(path: &Path) -> Result<Option<Value>, EditorsError> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(EditorsError::InvalidFile),
    };
    let mut data = Vec::new();
    file.take(MAX_AUTH_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|_| EditorsError::InvalidFile)?;
    if data.len() as u64 > MAX_AUTH_BYTES {
        return Err(EditorsError::InvalidFile);
    }
    let data = json_with_comments(&data)?;
    serde_json::from_slice(&data)
        .map(Some)
        .map_err(|_| EditorsError::InvalidFile)
}

// Copilot CLI accepts comments and trailing commas in its configuration.
fn json_with_comments(data: &[u8]) -> Result<Vec<u8>, EditorsError> {
    let mut clean = data.to_vec();
    let mut index = 0;
    let mut quoted = false;
    while index < clean.len() {
        if quoted {
            match clean[index] {
                b'\\' => {
                    index += 2;
                    continue;
                }
                b'"' => quoted = false,
                _ => {}
            }
        } else if clean[index] == b'"' {
            quoted = true;
        } else if clean[index] == b'/' && clean.get(index + 1) == Some(&b'/') {
            while index < clean.len() && clean[index] != b'\n' {
                clean[index] = b' ';
                index += 1;
            }
            continue;
        } else if clean[index] == b'/' && clean.get(index + 1) == Some(&b'*') {
            clean[index] = b' ';
            clean[index + 1] = b' ';
            index += 2;
            while index + 1 < clean.len() && !(clean[index] == b'*' && clean[index + 1] == b'/') {
                clean[index] = b' ';
                index += 1;
            }
            if index + 1 >= clean.len() {
                return Err(EditorsError::InvalidFile);
            }
            clean[index] = b' ';
            clean[index + 1] = b' ';
            index += 2;
            continue;
        }
        index += 1;
    }
    index = 0;
    quoted = false;
    while index < clean.len() {
        if quoted {
            match clean[index] {
                b'\\' => {
                    index += 2;
                    continue;
                }
                b'"' => quoted = false,
                _ => {}
            }
        } else if clean[index] == b'"' {
            quoted = true;
        } else if clean[index] == b',' {
            if clean[index + 1..]
                .iter()
                .find(|byte| !byte.is_ascii_whitespace())
                .is_some_and(|byte| matches!(*byte, b'}' | b']'))
            {
                clean[index] = b' ';
            }
        }
        index += 1;
    }
    Ok(clean)
}

fn valid_text(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 65_536 && !value.chars().any(char::is_control)
}
fn value_text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| valid_text(text))
}
fn text<'a>(credentials: &'a OAuthCredentials, key: &str) -> Option<&'a str> {
    credentials.extras.get(key).and_then(|value| value.as_str())
}

#[cfg(test)]
#[path = "../test/copilot_editors.test.rs"]
mod tests;
