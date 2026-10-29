use crate::settings::Settings;
use futures::future::{BoxFuture, Shared, WeakShared};
use futures::FutureExt;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tokio::io::AsyncReadExt;

type TokenFuture = BoxFuture<'static, PluginResult<String>>;
type PendingToken = (String, WeakShared<TokenFuture>);

#[derive(Clone)]
pub(crate) struct Gcloud {
    executable: PathBuf,
    environment: Arc<BTreeMap<String, String>>,
    tokens: Arc<Mutex<BTreeMap<String, PendingToken>>>,
    owners: Arc<tokio::sync::Mutex<BTreeSet<String>>>,
}
impl Default for Gcloud {
    fn default() -> Self {
        Self::new(
            PathBuf::from(if cfg!(windows) {
                "gcloud.cmd"
            } else {
                "gcloud"
            }),
            std::env::vars().collect(),
        )
    }
}
impl Gcloud {
    pub fn new(executable: PathBuf, environment: BTreeMap<String, String>) -> Self {
        Self {
            executable,
            environment: Arc::new(environment),
            tokens: Arc::default(),
            owners: Arc::default(),
        }
    }
    fn command(
        &self,
        args: &[String],
        settings: &Settings,
        quiet: bool,
    ) -> PluginResult<tokio::process::Command> {
        let mut args = args.to_vec();
        if quiet {
            args.push("--quiet".into());
        }
        #[cfg(not(windows))]
        let mut command = {
            let mut command = tokio::process::Command::new(&self.executable);
            command.args(&args);
            command
        };
        #[cfg(windows)]
        let mut command = {
            use std::os::windows::process::CommandExt;
            let file = self.executable.to_string_lossy();
            if std::iter::once(file.as_ref())
                .chain(args.iter().map(String::as_str))
                .any(|value| value.contains(['"', '\r', '\n']))
            {
                return Err(PluginError::invalid(
                    "An invalid gcloud argument was refused.",
                ));
            }
            let quoted = std::iter::once(file.as_ref())
                .chain(args.iter().map(String::as_str))
                .map(|value| format!("\"{value}\""))
                .collect::<Vec<_>>()
                .join(" ");
            let mut command = tokio::process::Command::new(
                self.environment
                    .get("ComSpec")
                    .map(String::as_str)
                    .unwrap_or("cmd.exe"),
            );
            command.args(["/d", "/s", "/c"]);
            command.as_std_mut().raw_arg(format!("\"{quoted}\""));
            command
        };
        command.env_clear().envs(
            self.environment
                .iter()
                .filter(|(key, _)| key.as_str() != "CLOUDSDK_CONFIG"),
        );
        if let Some(directory) = &settings.cloudsdk_config {
            command.env("CLOUDSDK_CONFIG", directory);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        Ok(command)
    }
    pub async fn run(&self, args: &[String], settings: &Settings) -> PluginResult<String> {
        let mut command = self.command(args, settings, true)?;
        let mut child=command.spawn().map_err(|error|if error.kind()==std::io::ErrorKind::NotFound {PluginError::authentication("gcloud CLI was not found; install Google Cloud CLI and make it available on PATH.")}else{command_failed(args)})?;
        tokio::time::timeout(Duration::from_secs(30),async {
            let mut output=Vec::new();
            child.stdout.take().unwrap().take(1024*1024+1).read_to_end(&mut output).await.map_err(|_|command_failed(args))?;
            if output.len()>1024*1024 {return Err(command_failed(args));}
            if !child.wait().await.map_err(|_|command_failed(args))?.success(){return Err(command_failed(args));}
            String::from_utf8(output).map(|output|output.trim().to_owned()).map_err(|_|command_failed(args))
        }).await.map_err(|_|PluginError::authentication("gcloud timed out while reading Google Cloud credentials; retry after checking connectivity."))?
    }
    pub async fn token(&self, settings: &Settings) -> PluginResult<String> {
        let mut effective = settings.clone();
        effective.auth_mode = crate::credentials::effective_mode(settings).into();
        let settings = &effective;
        let key = crate::cache::digest(&(
            settings.auth_mode.as_str(),
            &settings.account,
            &settings.cloudsdk_config,
        ));
        let (id, pending): (_, Shared<TokenFuture>) = {
            let mut tokens = self
                .tokens
                .lock()
                .map_err(|_| PluginError::transport("The Google token reader is busy."))?;
            if let Some((id, pending)) = tokens
                .get(&key)
                .and_then(|(id, pending)| pending.upgrade().map(|pending| (id.clone(), pending)))
            {
                (id, pending)
            } else {
                let id = uuid::Uuid::new_v4().to_string();
                let cli = self.clone();
                let settings = settings.clone();
                let pending=async move {
                    let args=if settings.auth_mode=="adc" {vec!["auth".into(),"application-default".into(),"print-access-token".into()]}else{
                        let account=crate::credentials::resolve_account(&cli,&settings).await?;
                        vec!["auth".into(),"print-access-token".into(),format!("--account={account}")]
                    };
                    let token=cli.run(&args,&settings).await?;
                    if token.is_empty()||token.chars().any(char::is_whitespace){return Err(PluginError::authentication("gcloud returned an empty or invalid access token. Sign in to Google Cloud again."));}
                    Ok(token)
                }.boxed().shared();
                if let Some(weak) = pending.downgrade() {
                    tokens.insert(key.clone(), (id.clone(), weak));
                }
                (id, pending)
            }
        };
        let result = pending.await;
        if let Ok(mut tokens) = self.tokens.lock() {
            if tokens.get(&key).is_some_and(|(current, _)| current == &id) {
                tokens.remove(&key);
            }
        }
        result
    }
    pub async fn verify_owner(
        &self,
        settings: &Settings,
        token: &str,
        client: &dyn nora_ai::HttpClient,
    ) -> PluginResult<()> {
        if crate::credentials::effective_mode(settings) != "adc"
            || (settings.account.is_none() && settings.expected_subject.is_none())
        {
            return Ok(());
        }
        let key = crate::cache::digest(&(
            crate::cache::digest(&token),
            &settings.account,
            &settings.expected_subject,
        ));
        let mut owners = self.owners.lock().await;
        if owners.contains(&key) {
            return Ok(());
        }
        let claims=crate::credentials::json_request(client,crate::credentials::authorized(nora_ai::HttpMethod::Get,"https://openidconnect.googleapis.com/v1/userinfo",token,None),Duration::from_secs(10)).await.map_err(|_|PluginError::transport("The Google account identity could not be confirmed. Credentials were kept; try again."))?;
        let email = claims
            .get("email")
            .and_then(serde_json::Value::as_str)
            .filter(|email| crate::settings::validate_account(email).is_ok());
        let subject = claims
            .get("sub")
            .and_then(serde_json::Value::as_str)
            .filter(|subject| !subject.is_empty());
        if (settings.account.is_some() && email.is_none())
            || settings
                .expected_subject
                .as_deref()
                .is_some_and(|expected| {
                    if crate::settings::validate_account(expected).is_ok() {
                        email.is_none()
                    } else {
                        subject.is_none()
                    }
                })
        {
            return Err(PluginError::transport("The Google account identity could not be confirmed. Credentials were kept; try again."));
        }
        let account_matches = settings
            .account
            .as_deref()
            .is_none_or(|expected| email.is_some_and(|email| email.eq_ignore_ascii_case(expected)));
        let subject_matches = settings.expected_subject.as_deref().is_none_or(|expected| {
            if crate::settings::validate_account(expected).is_ok() {
                email.is_some_and(|email| email.eq_ignore_ascii_case(expected))
            } else {
                subject == Some(expected)
            }
        });
        if email.is_none() && subject.is_none() {
            return Err(PluginError::transport("The Google account identity could not be confirmed. Credentials were kept; try again."));
        }
        if !account_matches || !subject_matches {
            return Err(PluginError::authentication(
                "This ADC now belongs to another Google account. Reconnect the saved account.",
            ));
        }
        if owners.len() >= 128 {
            owners.clear();
        }
        owners.insert(key);
        Ok(())
    }
    pub async fn begin_adc(&self, settings: &Settings) -> PluginResult<Consent> {
        let file = crate::credentials::adc_file(settings);
        let before = std::fs::metadata(&file)
            .and_then(|metadata| metadata.modified())
            .ok();
        let mut args = vec!["auth".into(), "application-default".into(), "login".into()];
        if let Some(project) = &settings.project {
            args.extend([
                format!("--project={project}"),
                format!("--billing-project={project}"),
            ]);
        }
        let mut command = self.command(&args, settings, false)?;
        command
            .env(
                "BROWSER",
                if cfg!(windows) {
                    "cmd /c rem %s"
                } else {
                    "true"
                },
            )
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.as_std_mut().process_group(0);
        }
        let mut child = command.spawn().map_err(|_| {
            PluginError::authentication("gcloud CLI could not be started to set up ADC.")
        })?;
        let pid = child.id();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let scanner = Arc::new(Mutex::new(Scanner {
            bytes: Vec::new(),
            sender: Some(sender),
        }));
        let out = read_consent(child.stdout.take().unwrap(), scanner.clone());
        let err = read_consent(child.stderr.take().unwrap(), scanner.clone());
        drop(scanner);
        let observed = file.clone();
        let task = tokio::spawn(async move {
            let mut out = AbortReader(tokio::spawn(out));
            let mut err = AbortReader(tokio::spawn(err));
            tokio::time::timeout(Duration::from_secs(600), async {
                let status = child.wait().await.map_err(|_| {
                    PluginError::authentication("gcloud could not complete ADC sign-in.")
                })?;
                let _ = tokio::time::timeout(Duration::from_secs(2), async {
                    let _ = (&mut out.0).await;
                    let _ = (&mut err.0).await;
                })
                .await;
                if !status.success() && !written_after(&observed, before) {
                    return Err(PluginError::authentication(
                        "gcloud could not set up ADC from the Chrome sign-in. Sign in again.",
                    ));
                }
                Ok(())
            })
            .await
            .map_err(|_| {
                PluginError::authentication("Google sign-in did not finish within ten minutes.")
            })?
        });
        let mut consent = Consent {
            task: Some(task),
            #[cfg(test)]
            file,
            before,
            url: String::new(),
            pid,
        };
        let url = tokio::time::timeout(Duration::from_secs(60), receiver).await;
        match url {
            Ok(Ok(url)) => {
                consent.url = url;
                Ok(consent)
            }
            _ => {
                consent.cancel().await;
                Err(PluginError::authentication(
                    "gcloud did not provide a consent page; no new account was added.",
                ))
            }
        }
    }
}
fn command_failed(args: &[String]) -> PluginError {
    let mode = if args.iter().any(|arg| arg == "application-default") {
        "gcloud auth application-default login"
    } else {
        "gcloud auth login"
    };
    PluginError::authentication(format!("gcloud could not read Google Cloud credentials or configuration. Run {mode} in a terminal and check the active gcloud configuration."))
}
fn written_after(path: &Path, before: Option<SystemTime>) -> bool {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .is_some_and(|time| before.is_none_or(|before| time > before))
}
struct AbortReader(tokio::task::JoinHandle<()>);
impl Drop for AbortReader {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct Scanner {
    bytes: Vec<u8>,
    sender: Option<tokio::sync::oneshot::Sender<String>>,
}
impl Scanner {
    fn scan(&mut self, bytes: &[u8]) {
        if self.sender.is_none() {
            return;
        }
        if self.bytes.len() + bytes.len() > 1024 * 1024 {
            self.sender.take();
            return;
        }
        self.bytes.extend_from_slice(bytes);
        let text = String::from_utf8_lossy(&self.bytes);
        let Some(start) = text.find("https://accounts.google.com/o/oauth2/auth?") else {
            return;
        };
        let Some(end) = text[start..].find(char::is_whitespace) else {
            return;
        };
        let url = text[start..start + end].to_owned();
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(url);
        }
    }
}
async fn read_consent(mut reader: impl tokio::io::AsyncRead + Unpin, scanner: Arc<Mutex<Scanner>>) {
    let mut bytes = [0; 4096];
    loop {
        match reader.read(&mut bytes).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                if let Ok(mut scanner) = scanner.lock() {
                    scanner.scan(&bytes[..read]);
                }
            }
        }
    }
}
pub(crate) struct Consent {
    task: Option<tokio::task::JoinHandle<PluginResult<()>>>,
    pub url: String,
    #[cfg(test)]
    pub file: PathBuf,
    before: Option<SystemTime>,
    pid: Option<u32>,
}
impl Consent {
    pub fn created_file(&self) -> bool {
        self.before.is_none()
    }
    pub async fn finish(&mut self) -> PluginResult<()> {
        let result = self
            .task
            .as_mut()
            .ok_or_else(|| PluginError::invalid("This Google sign-in already finished."))?
            .await
            .map_err(|_| PluginError::authentication("Google sign-in was stopped."));
        self.task.take();
        result?
    }
    fn stop(&self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
    pub async fn cancel(&mut self) {
        if let Some(task) = self.task.take() {
            self.stop();
            task.abort();
            let _ = task.await;
        }
    }
}
impl Drop for Consent {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            self.stop();
            task.abort();
        }
    }
}

#[cfg(test)]
#[path = "../test/cli.test.rs"]
mod tests;
