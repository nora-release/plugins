use super::proxy::Proxy;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
};
pub(crate) fn error() -> PluginError {
    PluginError::authentication(
        "Claude Code could not use this account. Sign in to this account again.",
    )
}
pub(crate) fn text(value: Option<&Value>, max: usize) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| {
            !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
        })
}
pub(crate) fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
pub(crate) fn random() -> String {
    nora_ai::oauth_pkce::random_hex(24)
}
pub(crate) fn executable(selected: Option<&str>) -> PluginResult<PathBuf> {
    if let Some(selected) = selected {
        let path = PathBuf::from(selected);
        if path.is_absolute() && path.is_file() {
            return Ok(path);
        }
        return Err(PluginError::invalid(
            "Select an installed Claude Code executable by its absolute path.",
        ));
    }
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        if directory.as_os_str().is_empty() {
            continue;
        }
        let path = directory.join(if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        });
        if path.is_file() {
            return Ok(path);
        }
    }
    Err(PluginError::authentication(
        "Install Claude Code, then select its executable or add claude to PATH.",
    ))
}
pub(crate) fn directory(path: &Path) -> PluginResult<()> {
    std::fs::create_dir_all(path).map_err(|_| error())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| error())?;
    }
    Ok(())
}
pub(crate) fn write(path: &Path, bytes: &[u8]) -> PluginResult<()> {
    use std::io::Write;
    let temporary = path.with_extension(format!("{}.tmp", random()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temporary).map_err(|_| error())?;
        file.write_all(bytes).map_err(|_| error())?;
        file.sync_all().map_err(|_| error())?;
        std::fs::rename(&temporary, path).map_err(|_| error())
    })();
    let _ = std::fs::remove_file(temporary);
    result
}
pub(crate) fn read(path: &Path) -> PluginResult<Value> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|_| error())?;
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| error())?;
    if bytes.len() > 1024 * 1024 {
        return Err(error());
    }
    serde_json::from_slice(&bytes).map_err(|_| error())
}
pub(crate) fn env(config: &Path, proxy: &Proxy, usage: bool) -> BTreeMap<String, String> {
    let blocked = [
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDECODE",
        "CLAUDE_CODE_ENTRYPOINT",
        "CLAUDE_CODE_SSE_PORT",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_EXTRA_BODY",
        "CLAUDE_CODE_PROMPT_CACHE_TTL",
        "ANTHROPIC_BETAS",
        "CLAUDE_CONFIG_DIR",
        "CLAUDE_SECURESTORAGE_CONFIG_DIR",
        "ENABLE_CLAUDEAI_MCP_SERVERS",
        "CLAUDE_CODE_DISABLE_AUTO_MEMORY",
        "BROWSER",
        "NORA_CLAUDE_OPENED_URL",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "ANTHROPIC_FOUNDRY_API_KEY",
        "ANTHROPIC_FOUNDRY_RESOURCE",
        "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
        "DISABLE_PROMPT_CACHING",
    ];
    let mut env = std::env::vars()
        .filter(|(key, _)| !blocked.contains(&key.as_str()) && !key.starts_with("CLAUDE_PLUGIN_"))
        .collect::<BTreeMap<_, _>>();
    env.extend(proxy.env.clone());
    for (key, value) in [
        ("CLAUDE_CONFIG_DIR", config.to_string_lossy().as_ref()),
        (
            "CLAUDE_SECURESTORAGE_CONFIG_DIR",
            config.to_string_lossy().as_ref(),
        ),
        ("ENABLE_CLAUDEAI_MCP_SERVERS", "0"),
        ("DISABLE_AUTO_COMPACT", "1"),
        ("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1"),
        ("CLAUDE_CODE_PROMPT_CACHE_TTL", "1h"),
        ("DISABLE_TELEMETRY", "1"),
        ("DISABLE_ERROR_REPORTING", "1"),
        ("DISABLE_AUTOUPDATER", "1"),
    ] {
        env.insert(key.into(), value.into());
    }
    if !usage {
        env.insert(
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(),
            "1".into(),
        );
    }
    env
}
pub(crate) struct Process {
    pub child: Child,
    #[cfg(unix)]
    _lifetime: super::supervisor::Lifetime,
}
impl Process {
    pub fn spawn(
        executable: &Path,
        args: &[String],
        cwd: &Path,
        environment: BTreeMap<String, String>,
    ) -> PluginResult<Self> {
        #[cfg(unix)]
        let mut command = Command::new(super::supervisor::executable().map_err(|_| error())?);
        #[cfg(not(unix))]
        let mut command = Command::new(executable);
        #[cfg(unix)]
        let mut lifetime =
            super::supervisor::configure(&mut command, executable, args).map_err(|_| error())?;
        #[cfg(not(unix))]
        command.args(args);
        command
            .current_dir(cwd)
            .env_clear()
            .envs(environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = command.spawn().map_err(|_| {
            PluginError::transport("The installed Claude Code executable could not start.")
        })?;
        #[cfg(unix)]
        lifetime.release_read();
        Ok(Self {
            child,
            #[cfg(unix)]
            _lifetime: lifetime,
        })
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        let _ = self.child.start_kill();
    }
}
#[derive(Clone)]
pub(crate) struct Account {
    pub executable: PathBuf,
    pub directory: PathBuf,
    pub identity: String,
    pub email: String,
    pub organization: String,
    pub plan: String,
    pub saved: Value,
}
impl Account {
    pub fn saved(&self) -> Value {
        let mut saved = self.saved.clone();
        saved["type"] = json!("oauth");
        saved["authMethod"] = json!("oauth");
        saved["provider"] = json!("anthropic");
        saved["credentialProvider"] = json!("anthropic");
        saved["accountId"] = json!(self.identity);
        saved["email"] = json!(self.email);
        saved["plan"] = json!(self.plan);
        saved["label"] = json!(if self.organization.is_empty() {
            self.email.clone()
        } else {
            format!("{} · {}", self.email, self.organization)
        });
        if !saved["extras"].is_object() {
            saved["extras"] = json!({});
        }
        saved["extras"]["noraClaudeCLI"] = json!({"directory":self.directory,"executable":self.executable,"identity":self.identity,"organization":self.organization});
        saved
    }
}
pub(crate) async fn output(
    executable: &Path,
    args: &[&str],
    cwd: &Path,
    proxy: Arc<Proxy>,
    usage: bool,
) -> PluginResult<Vec<u8>> {
    let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    let mut process = Process::spawn(executable, &args, cwd, env(cwd, &proxy, usage))?;
    drop(process.child.stdin.take());
    let mut stdout = process.child.stdout.take().ok_or_else(error)?;
    let mut stderr = process.child.stderr.take().ok_or_else(error)?;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(if usage { 40 } else { 20 }),
        async {
            let read = async {
                let mut bytes = Vec::new();
                stdout
                    .take(4 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .await
                    .map_err(|_| error())?;
                if bytes.len() > 4 * 1024 * 1024 {
                    return Err(error());
                }
                Ok(bytes)
            };
            let drain = async {
                let mut ignored = [0; 4096];
                while let Ok(size) = stderr.read(&mut ignored).await {
                    if size == 0 {
                        break;
                    }
                }
            };
            let (bytes, ()) = tokio::join!(read, drain);
            let status = process.child.wait().await.map_err(|_| error())?;
            if !status.success() {
                return Err(error());
            }
            bytes
        },
    )
    .await
    .map_err(|_| PluginError::transport("Claude Code did not reply in time."))?;
    result
}
pub(crate) async fn credentials(directory: &Path) -> PluginResult<Value> {
    #[cfg(target_os = "macos")]
    {
        use unicode_normalization::UnicodeNormalization;
        let path = directory.to_string_lossy().nfc().collect::<String>();
        let service = format!("Claude Code-credentials-{}", &hash(&path)[..8]);
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            Command::new("/usr/bin/security")
                .args(["find-generic-password", "-s", &service, "-w"])
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| error())?
        .map_err(|_| error())?;
        if output.status.success() && output.stdout.len() <= 1024 * 1024 {
            if let Ok(value) = serde_json::from_slice::<Value>(&output.stdout) {
                if value.get("claudeAiOauth").is_some() {
                    return Ok(value["claudeAiOauth"].clone());
                }
            }
        }
    }
    if let Ok(value) = read(&directory.join(".credentials.json")) {
        if value.get("claudeAiOauth").is_some() {
            return Ok(value["claudeAiOauth"].clone());
        }
    }

    Err(error())
}
pub(crate) async fn identify(
    executable: &Path,
    directory: &Path,
    proxy: Arc<Proxy>,
    expected: Option<&str>,
) -> PluginResult<Account> {
    let bytes = output(
        executable,
        &["auth", "status", "--json"],
        directory,
        proxy,
        false,
    )
    .await?;
    let status: Value = serde_json::from_slice(&bytes).map_err(|_| error())?;
    if status["loggedIn"] == false
        || status["authMethod"]
            .as_str()
            .is_some_and(|method| ["api_key", "apiKey", "console"].contains(&method))
    {
        return Err(error());
    }
    let profile = read(&directory.join(".claude.json")).unwrap_or_else(|_| json!({}));
    let email = text(status.get("email"), 512)
        .or_else(|| text(profile.pointer("/oauthAccount/emailAddress"), 512))
        .ok_or_else(error)?
        .to_owned();
    let account = text(status.get("accountUuid"), 256)
        .or_else(|| text(profile.pointer("/oauthAccount/accountUuid"), 256))
        .unwrap_or(&email);
    let organization = text(status.get("orgUuid").or_else(|| status.get("orgId")), 256)
        .or_else(|| text(profile.pointer("/oauthAccount/organizationUuid"), 256))
        .unwrap_or_default();
    let identity = hash(&format!(
        "claude-account\0{}\0{}",
        account.to_ascii_lowercase(),
        organization
    ));
    if expected.is_some_and(|expected| expected != identity) {
        return Err(error());
    }
    let oauth = credentials(directory).await?;
    let access = text(oauth.get("accessToken"), 64 * 1024).ok_or_else(error)?;
    let refresh = text(oauth.get("refreshToken"), 64 * 1024).ok_or_else(error)?;
    let plan = text(status.get("subscriptionType"), 128)
        .or_else(|| text(oauth.get("subscriptionType"), 128))
        .unwrap_or_default()
        .to_owned();
    let saved = json!({"access":access,"refresh":refresh,"expires":oauth["expiresAt"],"extras":{"scopes":oauth["scopes"]}});
    Ok(Account {
        executable: executable.into(),
        directory: directory.into(),
        identity,
        email,
        organization: text(status.get("orgName"), 256)
            .or_else(|| text(profile.pointer("/oauthAccount/organizationName"), 256))
            .unwrap_or_default()
            .into(),
        plan,
        saved,
    })
}
pub(crate) fn materialize(
    root: &Path,
    account: &str,
    value: &Value,
) -> PluginResult<(PathBuf, PathBuf, Option<String>)> {
    if let Some(reference) = value.pointer("/extras/noraClaudeCLI") {
        let directory = text(reference.get("directory"), 16 * 1024).ok_or_else(error)?;
        let executable = executable(text(reference.get("executable"), 16 * 1024))?;
        let directory = PathBuf::from(directory);
        if !directory.canonicalize().is_ok_and(|directory| {
            root.canonicalize()
                .is_ok_and(|root| directory.starts_with(root))
        }) || !directory.is_dir()
        {
            return Err(error());
        }
        let expected = text(reference.get("identity"), 128)
            .ok_or_else(error)?
            .to_owned();
        return Ok((executable, directory, Some(expected)));
    }
    let executable = executable(value.pointer("/extras/claudePath").and_then(Value::as_str))?;
    let directory = root.join(format!("account-{}", &hash(account)[..32]));
    self::directory(&directory)?;
    if !directory.join(".credentials.json").exists() {
        let access = text(
            value.get("access").or_else(|| value.get("accessToken")),
            64 * 1024,
        )
        .ok_or_else(error)?;
        let refresh = text(
            value.get("refresh").or_else(|| value.get("refreshToken")),
            64 * 1024,
        )
        .ok_or_else(error)?;
        let oauth = json!({"claudeAiOauth":{"accessToken":access,"refreshToken":refresh,"expiresAt":value["expires"],"scopes":["user:inference","user:profile"],"subscriptionType":value.get("plan").cloned().or_else(||value.pointer("/extras/plan").cloned()).unwrap_or(Value::Null)}});
        write(
            &directory.join(".credentials.json"),
            &serde_json::to_vec(&oauth).map_err(|_| error())?,
        )?;
        let email = text(
            value
                .get("email")
                .or_else(|| value.pointer("/extras/email")),
            512,
        )
        .ok_or_else(error)?;
        write(
            &directory.join(".claude.json"),
            &serde_json::to_vec(
                &json!({"hasCompletedOnboarding":true,"oauthAccount":{"emailAddress":email}}),
            )
            .map_err(|_| error())?,
        )?;
    }
    Ok((executable, directory, None))
}
pub(crate) fn opened_url(args: &[String]) -> bool {
    let Some(path) = std::env::var_os("NORA_CLAUDE_OPENED_URL") else {
        return false;
    };
    if args.len() != 1 {
        return false;
    }
    let Ok(url) = url::Url::parse(&args[0]) else {
        return false;
    };
    if !login_url(&url) {
        return false;
    }
    let _ = write(Path::new(&path), args[0].as_bytes());
    true
}
pub(crate) fn login_url(url: &url::Url) -> bool {
    url.scheme() == "https"
        && url.host_str().is_some_and(|host| {
            ["claude.ai", "console.anthropic.com", "platform.claude.com"].contains(&host)
        })
        && url.path().contains("oauth")
        && url.username().is_empty()
        && url.password().is_none()
}
#[cfg(test)]
#[path = "../test/cli.test.rs"]
mod tests;
