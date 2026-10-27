use crate::xai_grok_client_version;
use base64::Engine;
use nora_ai::{HttpClient, JsonValue, OAuthCredentials, OAuthProviderId};
use nora_provider_plugin_sdk::NetworkProxy;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

pub const METADATA_KEY: &str = "noraAccountSource";
pub const ACCOUNT_ID_KEY: &str = "noraGrokCLIAccountUserID";
pub const VERSION_KEY: &str = "grokClientVersion";
const ISSUER: &str = "https://auth.x.ai";
const SCOPE: &str = "https://auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828";
const MAX_AUTH_BYTES: u64 = 1_048_576;
const TORN_READ_ATTEMPTS: usize = 6;
const TORN_READ_WAIT: Duration = Duration::from_millis(100);

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Source {
    kind: String,
    home_directory: PathBuf,
    auth_file_path: PathBuf,
    executable_path: Option<PathBuf>,
}

#[derive(Clone)]
struct Session {
    token: String,
    expires_at: Option<i64>,
}

#[derive(Clone)]
struct SourceCredential {
    user: String,
    session: Session,
}

#[derive(Debug, Eq, PartialEq)]
pub enum SourceError {
    InvalidSource,
    NotSignedIn,
    InvalidCredentials,
    AccountChanged,
    CliUnavailable,
    ReadFailed,
    RefreshFailed,
    RenewalUnchanged,
    Expired,
    TimedOut,
    InvalidNetworkProxy,
}

pub fn is_bound(credentials: &OAuthCredentials) -> bool {
    credentials.provider == OAuthProviderId::XAI && credentials.extras.contains_key(METADATA_KEY)
}

pub fn client_version(credentials: &OAuthCredentials) -> &str {
    let minimum = xai_grok_client_version();
    credentials
        .extras
        .get(VERSION_KEY)
        .and_then(|value| value.as_str())
        .filter(|version| {
            semver(version)
                .zip(semver(minimum))
                .is_some_and(|(version, minimum)| version >= minimum)
        })
        .unwrap_or(minimum)
}

fn semver(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.');
    let result = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(result)
}

pub async fn bind_current_account() -> Result<OAuthCredentials, String> {
    let client = nora_provider_network::http_client(None)
        .map(|client| {
            nora_ai::ReqwestHttpClient::with_client(client).with_observer_fn(|| async {
                nora_provider_plugin_sdk::before_http_request()
                    .await
                    .map_err(|error| nora_ai::HttpClientError::Transport(error.to_string()))
            })
        })
        .map_err(|_| "The Grok Build HTTP client could not be initialized.".to_string())?;
    bind_current_account_with_client(&client).await
}

pub async fn bind_current_account_with_client(
    client: &dyn HttpClient,
) -> Result<OAuthCredentials, String> {
    bind_current_account_with_network(client, None).await
}

pub async fn bind_current_account_with_network(
    client: &dyn HttpClient,
    proxy: Option<&NetworkProxy>,
) -> Result<OAuthCredentials, String> {
    nora_provider_plugin_sdk::require_subscription_accounts().map_err(|error| error.message)?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or("The current user's home directory is unavailable")?;
    let source = Source::current(&home, &std::env::vars().collect());
    bind_source(source, client, proxy).await
}

/// Discovers the current local sign-in without treating a read or network
/// failure as a sign-out. The saved account remains a token-free reference.
pub async fn discover_current_account_with_network(
    client: &dyn HttpClient,
    proxy: Option<&NetworkProxy>,
) -> Result<Option<OAuthCredentials>, String> {
    nora_provider_plugin_sdk::require_subscription_accounts().map_err(|error| error.message)?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or("The current user's home directory is unavailable")?;
    discover_source(
        Source::current(&home, &std::env::vars().collect()),
        client,
        proxy,
    )
    .await
}

async fn discover_source(
    source: Source,
    client: &dyn HttpClient,
    _proxy: Option<&NetworkProxy>,
) -> Result<Option<OAuthCredentials>, String> {
    let mut credentials =
        match source.bound_credentials().await {
            Ok(credentials) => credentials,
            Err(SourceError::NotSignedIn) => return Ok(None),
            Err(_) => return Err(
                "The Grok Build sign-in could not be read. Try again after checking Grok Build."
                    .into(),
            ),
        };
    let subject = credentials
        .extras
        .get(ACCOUNT_ID_KEY)
        .and_then(|value| value.as_str())
        .ok_or("The Grok Build sign-in did not identify its account.")?;
    let session = source.read(subject).await.map_err(|_| {
        "The Grok Build sign-in could not be read. Try again after checking Grok Build."
    })?;
    // Discovery does not run a CLI command or renew a third-party session.
    if session
        .expires_at
        .is_some_and(|expires| expires <= now_millis())
    {
        return Err("The Grok Build sign-in expired. Open Grok Build to renew it.".into());
    }
    let user = crate::validate_xai_oauth_session(client, &session.token)
        .await
        .map_err(|_| "xAI could not verify the Grok Build account. Try again.")?;
    if user.user_id != subject {
        return Err("The Grok Build account changed while it was being read. Try again.".into());
    }
    credentials.extras.extend(user.credential_extras());
    credentials.expires_at = None;
    Ok(Some(credentials))
}

async fn bind_source(
    source: Source,
    client: &dyn HttpClient,
    proxy: Option<&NetworkProxy>,
) -> Result<OAuthCredentials, String> {
    let mut credentials = source.bound_credentials().await.map_err(|error| {
        if matches!(
            error,
            SourceError::NotSignedIn | SourceError::InvalidCredentials
        ) {
            "No valid Grok Build account was found. Sign in with Grok Build and try again."
        } else {
            "The Grok Build sign-in could not be read. Try again after checking Grok Build."
        }
        .to_string()
    })?;
    let transient = resolve_credentials_with_network(&credentials, false, proxy)
        .await
        .map_err(|_| {
            "The Grok Build account could not be renewed. Open Grok Build and try again."
        })?;
    let user = crate::validate_xai_oauth_session(client, &transient.access_token)
        .await
        .map_err(|_| "xAI could not verify the Grok Build account. Try again.")?;
    if credentials
        .extras
        .get(ACCOUNT_ID_KEY)
        .and_then(|value| value.as_str())
        != Some(user.user_id.as_str())
    {
        return Err(
            "Grok Build is using a different account. Reconnect the account in Nora.".into(),
        );
    }
    credentials.extras.extend(user.credential_extras());
    if let Some(version) = transient.extras.get(VERSION_KEY) {
        credentials
            .extras
            .insert(VERSION_KEY.into(), version.clone());
    }
    credentials.expires_at = None;
    Ok(credentials)
}

/// Reads a fresh session for the bound local account without changing the saved reference.
pub async fn resolve_credentials(
    credentials: &OAuthCredentials,
    force: bool,
) -> Result<OAuthCredentials, SourceError> {
    resolve_credentials_with_network(credentials, force, None).await
}

pub async fn resolve_credentials_with_network(
    credentials: &OAuthCredentials,
    force: bool,
    proxy: Option<&NetworkProxy>,
) -> Result<OAuthCredentials, SourceError> {
    nora_provider_plugin_sdk::require_subscription_accounts()
        .map_err(|_| SourceError::InvalidSource)?;
    if let Some(proxy) = proxy {
        proxy
            .validate()
            .map_err(|_| SourceError::InvalidNetworkProxy)?;
    }
    let source: Source = credentials
        .extras
        .get(METADATA_KEY)
        .and_then(|value| serde_json::from_value(value.as_value().clone()).ok())
        .ok_or(SourceError::InvalidSource)?;
    if source.kind != "grokCLI"
        || !source.home_directory.is_absolute()
        || !source.auth_file_path.is_absolute()
        || source.executable_path.as_ref().is_some_and(|path| {
            !path.is_absolute() || path.file_name().map_or(true, |name| name != "grok")
        })
    {
        return Err(SourceError::InvalidSource);
    }
    let user_id = credentials
        .extras
        .get(ACCOUNT_ID_KEY)
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or(SourceError::InvalidSource)?;
    let observed = source.read(user_id).await?;
    let lock = source_lock(&source);
    let _guard = lock.lock().await;
    let mut session = source.read(user_id).await?;
    let force_refresh = force && session.token == observed.token;
    if force_refresh
        || session
            .expires_at
            .is_some_and(|expires| expires <= now_millis().saturating_add(300_000))
    {
        let renewal = source
            .command(&["models"], Duration::from_secs(30), false, proxy)
            .await;
        // The CLI can save renewed credentials before a later command failure.
        // Its exit status alone cannot tell whether the session is usable.
        session = source.read(user_id).await?;
        if force_refresh && session.token == observed.token {
            return Err(renewal.err().unwrap_or(SourceError::RenewalUnchanged));
        }
        if session
            .expires_at
            .is_some_and(|expires| expires <= now_millis())
        {
            return Err(renewal.err().unwrap_or(SourceError::Expired));
        }
    }
    if session
        .expires_at
        .is_some_and(|expires| expires <= now_millis())
    {
        return Err(SourceError::Expired);
    }
    let version = source.version(proxy).await;
    let mut transient = credentials.clone();
    transient.access_token = session.token;
    transient.refresh_token = None;
    transient.expires_at = session.expires_at;
    transient
        .extras
        .insert(VERSION_KEY.into(), JsonValue::string(version));
    Ok(transient)
}

impl Source {
    fn current(home: &std::path::Path, environment: &BTreeMap<String, String>) -> Self {
        let path = |key: &str, relative: &std::path::Path| {
            environment
                .get(key)
                .filter(|value| !value.trim().is_empty())
                .map(|value| {
                    if value == "~" {
                        return relative.to_path_buf();
                    }
                    relative.join(value.strip_prefix("~/").unwrap_or(value))
                })
        };
        let grok_home = path("GROK_HOME", home).unwrap_or_else(|| home.join(".grok"));
        let auth_file_path =
            path("GROK_AUTH_PATH", &grok_home).unwrap_or_else(|| grok_home.join("auth.json"));
        let mut executables = Vec::new();
        if let Some(bin) = path("GROK_BIN_DIR", home) {
            executables.push((bin.join("grok"), true));
        }
        executables.push((grok_home.join("bin/grok"), true));
        executables.push((home.join(".grok/bin/grok"), true));
        for bin in environment
            .get("PATH")
            .into_iter()
            .flat_map(|path| std::env::split_paths(path))
        {
            if bin.is_absolute() {
                executables.push((bin.join("grok"), false));
            }
        }
        executables.push((home.join(".local/bin/grok"), false));
        let executable_path = executables.into_iter().find_map(|(path, trusted)| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let executable = std::fs::metadata(&path).is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                });
                if executable
                    && (trusted
                        || path.canonicalize().is_ok_and(|real| {
                            real.components().any(|part| part.as_os_str() == ".grok")
                        }))
                {
                    return Some(path);
                }
            }
            None
        });
        Self {
            kind: "grokCLI".into(),
            home_directory: grok_home,
            auth_file_path,
            executable_path,
        }
    }

    async fn bound_credentials(&self) -> Result<OAuthCredentials, SourceError> {
        let credential = self.read_current().await?;
        Ok(OAuthCredentials::new(
            OAuthProviderId::XAI,
            "",
            None,
            credential.session.expires_at,
            BTreeMap::from([
                (
                    METADATA_KEY.into(),
                    serde_json::to_value(self)
                        .map_err(|_| SourceError::InvalidSource)?
                        .into(),
                ),
                (ACCOUNT_ID_KEY.into(), JsonValue::string(&credential.user)),
                ("userId".into(), JsonValue::string(&credential.user)),
            ]),
        ))
    }

    async fn read(&self, expected_user: &str) -> Result<Session, SourceError> {
        let credential = self.read_current().await?;
        if credential.user != expected_user {
            return Err(SourceError::AccountChanged);
        }
        Ok(credential.session)
    }

    async fn read_current(&self) -> Result<SourceCredential, SourceError> {
        for attempt in 0..TORN_READ_ATTEMPTS {
            match self.look() {
                Ok(Some(credential)) => {
                    remember_credential(&self.auth_file_path, Some(&credential));
                    return Ok(credential);
                }
                Ok(None) if attempt + 1 < TORN_READ_ATTEMPTS => {
                    tokio::time::sleep(TORN_READ_WAIT).await;
                }
                Ok(None) => break,
                Err(error) => {
                    if matches!(
                        error,
                        SourceError::NotSignedIn | SourceError::InvalidCredentials
                    ) {
                        remember_credential(&self.auth_file_path, None);
                    }
                    return Err(error);
                }
            }
        }
        // Grok Build rewrites this file during renewal. Only an incomplete
        // JSON read may use the last whole record from this exact file.
        credential_cache()
            .lock()
            .unwrap()
            .get(&self.auth_file_path)
            .map(|(_, credential)| credential.clone())
            .ok_or(SourceError::ReadFailed)
    }

    fn look(&self) -> Result<Option<SourceCredential>, SourceError> {
        let file = std::fs::File::open(&self.auth_file_path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                SourceError::NotSignedIn
            } else {
                SourceError::ReadFailed
            }
        })?;
        let mut data = Vec::new();
        file.take(MAX_AUTH_BYTES + 1)
            .read_to_end(&mut data)
            .map_err(|_| SourceError::ReadFailed)?;
        if data.len() as u64 > MAX_AUTH_BYTES {
            return Err(SourceError::ReadFailed);
        }
        let Ok(all) = serde_json::from_slice::<Value>(&data) else {
            return Ok(None);
        };
        let credential = all
            .get(SCOPE)
            .or_else(|| all.get(ISSUER))
            .and_then(Value::as_object)
            .ok_or(SourceError::NotSignedIn)?;
        let token = credential
            .get("key")
            .and_then(Value::as_str)
            .filter(|token| !token.trim().is_empty())
            .ok_or(SourceError::NotSignedIn)?;
        let claims = jwt_claims(token);
        for issuer in [
            credential.get("oidc_issuer"),
            claims.as_ref().and_then(|claims| claims.get("iss")),
        ]
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        {
            if issuer != ISSUER {
                return Err(SourceError::InvalidCredentials);
            }
        }
        let subject = claims
            .as_ref()
            .and_then(|claims| claims.get("sub"))
            .and_then(Value::as_str);
        let user = credential
            .get("user_id")
            .and_then(Value::as_str)
            .or(subject)
            .map(str::trim)
            .filter(|user| !user.is_empty())
            .ok_or(SourceError::InvalidCredentials)?;
        if subject.is_some_and(|subject| subject != user) {
            return Err(SourceError::InvalidCredentials);
        }
        let mut expires_at = claims
            .as_ref()
            .and_then(|claims| claims.get("exp"))
            .and_then(Value::as_f64)
            .filter(|seconds| seconds.is_finite())
            .map(|seconds| (seconds * 1000.0) as i64);
        if let Some(raw) = credential.get("expires_at").and_then(Value::as_str) {
            let date = chrono::DateTime::parse_from_rfc3339(raw)
                .map_err(|_| SourceError::InvalidCredentials)?
                .timestamp_millis();
            expires_at = Some(expires_at.map_or(date, |expires| expires.min(date)));
        }
        Ok(Some(SourceCredential {
            user: user.to_string(),
            session: Session {
                token: token.to_string(),
                expires_at,
            },
        }))
    }

    async fn command(
        &self,
        arguments: &[&str],
        timeout: Duration,
        capture: bool,
        proxy: Option<&NetworkProxy>,
    ) -> Result<String, SourceError> {
        if let Some(proxy) = proxy {
            proxy
                .validate()
                .map_err(|_| SourceError::InvalidNetworkProxy)?;
        }
        let executable = self
            .executable_path
            .as_ref()
            .ok_or(SourceError::CliUnavailable)?;
        let mut command = Command::new(executable);
        command
            .args(arguments)
            .env_clear()
            .envs(command_environment(self, std::env::vars_os(), proxy))
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(if capture {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .kill_on_drop(true);
        if let Some(parent) = self.home_directory.parent() {
            command.current_dir(parent);
        }
        let mut child = command.spawn().map_err(|_| SourceError::CliUnavailable)?;
        tokio::time::timeout(timeout, async {
            let mut output = Vec::new();
            if let Some(stdout) = child.stdout.take() {
                stdout
                    .take(4096)
                    .read_to_end(&mut output)
                    .await
                    .map_err(|_| SourceError::RefreshFailed)?;
            }
            let status = child.wait().await.map_err(|_| SourceError::RefreshFailed)?;
            if !status.success() {
                return Err(SourceError::RefreshFailed);
            }
            Ok(String::from_utf8_lossy(&output).into_owned())
        })
        .await
        .map_err(|_| SourceError::TimedOut)?
    }

    async fn version(&self, proxy: Option<&NetworkProxy>) -> String {
        type Cache = Mutex<BTreeMap<PathBuf, (Instant, String)>>;
        static CACHE: OnceLock<Cache> = OnceLock::new();
        let Some(executable) = self.executable_path.as_ref() else {
            return xai_grok_client_version().into();
        };
        let cache = CACHE.get_or_init(Mutex::default);
        if let Some((at, version)) = cache.lock().unwrap().get(executable) {
            if at.elapsed() < Duration::from_secs(600) {
                return version.clone();
            }
        }
        let version = self
            .command(&["--version"], Duration::from_secs(5), true, proxy)
            .await
            .ok()
            .and_then(|output| {
                output
                    .split_whitespace()
                    .find(|part| semver(part).is_some())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| xai_grok_client_version().into());
        cache
            .lock()
            .unwrap()
            .insert(executable.clone(), (Instant::now(), version.clone()));
        version
    }
}

type CredentialCache = Mutex<BTreeMap<PathBuf, (Instant, SourceCredential)>>;

fn credential_cache() -> &'static CredentialCache {
    static CACHE: OnceLock<CredentialCache> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

fn remember_credential(path: &std::path::Path, credential: Option<&SourceCredential>) {
    let mut cache = credential_cache().lock().unwrap();
    let Some(credential) = credential else {
        cache.remove(path);
        return;
    };
    cache.insert(path.to_path_buf(), (Instant::now(), credential.clone()));
    // A long-lived plugin should not retain an unbounded number of old tokens.
    if cache.len() > 128 {
        if let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, (time, _))| time)
            .map(|(path, _)| path.clone())
        {
            cache.remove(&oldest);
        }
    }
}

fn command_environment(
    source: &Source,
    inherited: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    proxy: Option<&NetworkProxy>,
) -> BTreeMap<std::ffi::OsString, std::ffi::OsString> {
    let mut environment = inherited
        .into_iter()
        .filter(|(key, _)| {
            !matches!(
                key.to_string_lossy().to_ascii_uppercase().as_str(),
                "GROK_HOME"
                    | "GROK_AUTH_PATH"
                    | "GROK_AUTH_PROVIDER_COMMAND"
                    | "GROK_AUTH_EXPIRED"
                    | "GROK_AUTH_PROVIDER_REFRESH_TOKEN"
                    | "GROK_API_KEY"
                    | "XAI_API_KEY"
                    | "HTTP_PROXY"
                    | "HTTPS_PROXY"
                    | "ALL_PROXY"
                    | "NO_PROXY"
                    | "FTP_PROXY"
            )
        })
        .collect::<BTreeMap<_, _>>();
    environment.insert(
        "GROK_HOME".into(),
        source.home_directory.as_os_str().to_owned(),
    );
    environment.insert(
        "GROK_AUTH_PATH".into(),
        source.auth_file_path.as_os_str().to_owned(),
    );
    match proxy {
        Some(NetworkProxy::Proxy { url }) => {
            for name in [
                "HTTP_PROXY",
                "http_proxy",
                "HTTPS_PROXY",
                "https_proxy",
                "ALL_PROXY",
                "all_proxy",
            ] {
                environment.insert(name.into(), url.into());
            }
            for name in ["NO_PROXY", "no_proxy"] {
                environment.insert(name.into(), "".into());
            }
        }
        Some(NetworkProxy::Direct) | None => {
            // CLI probes and refreshes obey the same explicit policy as native HTTP calls.
            for name in ["NO_PROXY", "no_proxy"] {
                environment.insert(name.into(), "*".into());
            }
        }
    }
    environment
}

fn source_lock(source: &Source) -> Arc<tokio::sync::Mutex<()>> {
    type Locks = Mutex<BTreeMap<(PathBuf, PathBuf), Weak<tokio::sync::Mutex<()>>>>;
    static LOCKS: OnceLock<Locks> = OnceLock::new();
    let mut locks = LOCKS.get_or_init(Mutex::default).lock().unwrap();
    locks.retain(|_, lock| lock.strong_count() > 0);
    let key = (source.home_directory.clone(), source.auth_file_path.clone());
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    lock
}

fn jwt_claims(token: &str) -> Option<Value> {
    let parts = token.split('.').collect::<Vec<_>>();
    if parts.len() != 3 {
        return None;
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(parts[1]))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
#[path = "../test/grok_source.test.rs"]
mod tests;
