use super::{cli, proxy::Proxy};
use nora_provider_auth::browser_sign_in::BrowserSignIn;
use nora_provider_plugin_sdk::{NetworkProxy, PluginError, PluginResult};
use serde_json::{json, Value};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, watch, Mutex},
    task::{AbortHandle, JoinSet},
    time::Instant,
};

#[path = "login_directory.rs"]
mod directory;
use directory::Directory;

pub(crate) struct Pending {
    state: Mutex<State>,
    abort: AbortHandle,
    cancelled: watch::Sender<bool>,
    directory: Arc<Directory>,
    page: Page,
}
struct State {
    sign_in: BrowserSignIn<cli::Account>,
    input: Option<tokio::process::ChildStdin>,
    submitted: Option<String>,
}
#[derive(Clone)]
struct Page {
    url: String,
    state: Option<String>,
    callback: Option<url::Url>,
}
pub(crate) enum Reply {
    Waiting(Value),
    Complete(cli::Account),
    Claimed,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.cancelled.send_replace(true);
        self.abort.abort();
    }
}
impl Pending {
    pub fn authorization(&self, session: &str) -> Value {
        let callback = self.page.callback.is_some();
        json!({
            "session":session,"url":self.page.url,"method":if callback {"auto"} else {"code"},
            "callbackMode":"poll","pasteCallback":callback,"pasteCode":!callback,
            "cancelAuthorization":true
        })
    }

    pub async fn callback(&self, session: &str, params: &Value) -> PluginResult<Reply> {
        let mut cancelled = self.cancelled.subscribe();
        if *cancelled.borrow() {
            return Err(expired());
        }
        tokio::select! {
            _ = cancelled.wait_for(|cancelled| *cancelled) => Err(expired()),
            result = self.complete(session, params) => result,
        }
    }

    async fn complete(&self, session: &str, params: &Value) -> PluginResult<Reply> {
        let mut state = self.state.lock().await;
        if state.sign_in.completion_started() {
            return Ok(Reply::Claimed);
        }
        if !self.abort.is_finished() {
            if let Some(raw) = params
                .get("callbackURL")
                .or_else(|| params.get("code"))
                .or_else(|| params.get("response"))
            {
                let result = match raw.as_str() {
                    Some(raw) => self.submit(&mut state, raw).await,
                    None => Err(invalid_input()),
                };
                if let Err(error) = result {
                    return Ok(Reply::Waiting(
                        json!({"session":session,"pending":true,"inputError":error.message}),
                    ));
                }
            }
        }
        if let Some(waiting) = state.sign_in.waiting_reply(session, params["poll"] == true) {
            return Ok(Reply::Waiting(waiting));
        }
        let account = state.sign_in.finish().await?;
        Ok(Reply::Complete(account))
    }

    async fn submit(&self, state: &mut State, raw: &str) -> PluginResult<()> {
        let raw = raw.trim();
        if raw.is_empty()
            || raw.len() > 16 * 1024
            || raw.chars().any(char::is_whitespace)
            || raw.chars().any(char::is_control)
        {
            return Err(invalid_input());
        }
        let target = if raw.starts_with("http://") || raw.starts_with("https://") {
            Some(self.page.callback_target(raw)?)
        } else {
            let (code, returned_state) = raw.split_once('#').ok_or_else(invalid_input)?;
            if code.is_empty()
                || returned_state.is_empty()
                || returned_state.contains('#')
                || self
                    .page
                    .state
                    .as_deref()
                    .is_some_and(|expected| returned_state != expected)
            {
                return Err(invalid_input());
            }
            None
        };
        if let Some(submitted) = &state.submitted {
            return if submitted == raw {
                Ok(())
            } else {
                Err(PluginError::invalid("This sign-in is already finishing."))
            };
        }
        if let Some(target) = target {
            let client =
                nora_provider_plugin_sdk::http_client_builder(Some(&NetworkProxy::Direct))?
                    .no_proxy()
                    .connect_timeout(Duration::from_secs(5))
                    .timeout(Duration::from_secs(30))
                    .build()
                    .map_err(|_| callback_failed())?;
            let response = client
                .get(target)
                .send()
                .await
                .map_err(|_| callback_failed())?;
            if !response.status().is_success() {
                return Err(callback_failed());
            }
        } else {
            let input = state.input.as_mut().ok_or_else(expired)?;
            tokio::time::timeout(Duration::from_secs(5), async {
                input.write_all(format!("{raw}\n").as_bytes()).await?;
                input.flush().await
            })
            .await
            .map_err(|_| expired())?
            .map_err(|_| expired())?;
        }
        state.submitted = Some(raw.to_owned());
        Ok(())
    }

    pub fn keep(&self) {
        self.directory.keep();
    }

    pub async fn cancel(&self) {
        self.cancelled.send_replace(true);
        self.abort.abort();
        let mut state = self.state.lock().await;
        if !self.abort.is_finished() {
            let _ = (&mut state.sign_in.task).await;
        }
        state.input.take();
        self.directory.clear();
    }
}

impl Page {
    fn new(url: String, opened: bool) -> Self {
        let parsed = url::Url::parse(&url).expect("The CLI sign-in URL was validated");
        let state = parsed
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned());
        let callback = opened
            .then(|| {
                parsed
                    .query_pairs()
                    .find(|(key, _)| key == "redirect_uri")
                    .and_then(|(_, value)| url::Url::parse(&value).ok())
            })
            .flatten()
            .filter(|url| {
                matches!(url.scheme(), "http" | "https")
                    && loopback(url)
                    && url.port().is_some_and(|port| port > 0)
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.fragment().is_none()
            });
        Self {
            url,
            state,
            callback,
        }
    }

    fn callback_target(&self, raw: &str) -> PluginResult<url::Url> {
        let expected = self.callback.as_ref().ok_or_else(invalid_input)?;
        let returned = url::Url::parse(raw).map_err(|_| invalid_input())?;
        if !matches!(returned.scheme(), "http" | "https")
            || !loopback(&returned)
            || returned.port() != expected.port()
            || returned.path() != expected.path()
            || !returned.username().is_empty()
            || returned.password().is_some()
            || returned.fragment().is_some()
        {
            return Err(invalid_input());
        }
        let fields = returned.query_pairs().collect::<Vec<_>>();
        let codes = fields
            .iter()
            .filter(|(key, _)| key == "code")
            .collect::<Vec<_>>();
        let states = fields
            .iter()
            .filter(|(key, _)| key == "state")
            .collect::<Vec<_>>();
        if codes.len() != 1
            || codes[0].1.is_empty()
            || codes[0].1.len() > 8192
            || codes[0].1.chars().any(char::is_control)
            || states.len() != 1
            || self
                .state
                .as_deref()
                .is_some_and(|state| states[0].1 != state)
        {
            return Err(invalid_input());
        }
        // Only the listener advertised by this CLI receives the callback. A
        // pasted hostname, scheme or redirect never chooses the destination.
        let mut target = expected.clone();
        target.set_scheme("http").map_err(|_| invalid_input())?;
        target.set_query(returned.query());
        Ok(target)
    }
}
fn loopback(url: &url::Url) -> bool {
    matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    )
}
fn invalid_input() -> PluginError {
    PluginError::invalid("Paste the code from this Claude sign-in, or the complete callback address from its browser tab.")
}
fn expired() -> PluginError {
    PluginError::authentication("This Claude sign-in has ended. Start it again.")
}
fn callback_failed() -> PluginError {
    PluginError::invalid(
        "Claude Code could not accept that callback address. Check the address and try again.",
    )
}

async fn read(
    mut output: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    send: mpsc::Sender<String>,
) {
    let mut buffer = String::new();
    let mut sent = false;
    loop {
        let mut chunk = [0; 4096];
        let Ok(size) = output.read(&mut chunk).await else {
            break;
        };
        if size == 0 {
            break;
        }
        if sent {
            continue;
        }
        buffer.push_str(&String::from_utf8_lossy(&chunk[..size]));
        for word in buffer.split(|character: char| {
            character.is_whitespace() || character == '\x1b' || character == '"'
        }) {
            let candidate = word.trim_matches(['\'', ')', '(', '[', ']']);
            if let Ok(url) = url::Url::parse(candidate) {
                if cli::login_url(&url) {
                    let _ = send.send(url.to_string()).await;
                    sent = true;
                    break;
                }
            }
        }
        if buffer.len() > 64 * 1024 {
            buffer = buffer
                .chars()
                .rev()
                .take(32768)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
        }
    }
}

fn opened_page(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path)
        .ok()
        .filter(|bytes| bytes.len() <= 16 * 1024)?;
    let text = std::str::from_utf8(&bytes).ok()?.trim();
    let url = url::Url::parse(text).ok().filter(cli::login_url)?;
    Some(url.to_string())
}

async fn page(
    opened: &Path,
    receive: &mut mpsc::Receiver<String>,
    task: &AbortHandle,
) -> PluginResult<Page> {
    let mut printed = None;
    let mut deadline = Instant::now() + Duration::from_secs(30);
    let mut closed = false;
    loop {
        if let Some(url) = opened_page(opened) {
            return Ok(Page::new(url, true));
        }
        if task.is_finished() {
            if printed.is_none() {
                printed = receive.try_recv().ok();
            }
            return printed
                .map(|url| Page::new(url, false))
                .ok_or_else(cli::error);
        }
        tokio::select! {
            url = receive.recv(), if !closed => {
                if let Some(url) = url { printed = Some(url); deadline = Instant::now() + Duration::from_secs(3); }
                else { closed = true; }
            },
            _ = tokio::time::sleep_until(deadline) => {
                return printed.map(|url| Page::new(url, false)).ok_or_else(|| PluginError::authentication("Claude Code returned no sign-in page. Use its installed auth login command to check the installation."));
            },
            _ = tokio::time::sleep(Duration::from_millis(50)) => {},
        }
    }
}

pub(crate) async fn start(
    root: &Path,
    selected: Option<&str>,
    proxy: Arc<Proxy>,
) -> PluginResult<Arc<Pending>> {
    let executable = cli::executable(selected)?;
    let directory = Arc::new(Directory::new(
        root.join(format!("signin-{}", cli::random())),
    ));
    cli::directory(&directory.path)?;
    let opened = directory.path.join(".opened-url");
    let mut environment = cli::env(&cli::Home::Isolated(directory.path.clone()), &proxy, false);
    environment.insert(
        "BROWSER".into(),
        std::env::current_exe()
            .map_err(|_| cli::error())?
            .to_string_lossy()
            .into(),
    );
    environment.insert(
        "NORA_CLAUDE_OPENED_URL".into(),
        opened.to_string_lossy().into(),
    );
    let mut process = cli::Process::spawn(
        &executable,
        &["auth".into(), "login".into(), "--claudeai".into()],
        &directory.path,
        environment,
    )?;
    let input = process.child.stdin.take();
    let stdout = process.child.stdout.take().ok_or_else(cli::error)?;
    let stderr = process.child.stderr.take().ok_or_else(cli::error)?;
    let (send, mut receive) = mpsc::channel(2);
    let task_directory = directory.clone();
    let task = tokio::spawn(async move {
        let mut readers = JoinSet::new();
        readers.spawn(read(stdout, send.clone()));
        readers.spawn(read(stderr, send));
        tokio::time::timeout(Duration::from_secs(600), async {
            let status = process.child.wait().await.map_err(|_| cli::error())?;
            if !status.success() {
                return Err(cli::error());
            }
            cli::identify(
                &executable,
                &cli::Home::Isolated(task_directory.path.clone()),
                proxy,
                None,
            )
            .await
        })
        .await
        .map_err(|_| PluginError::authentication("Claude Code sign-in timed out."))?
    });
    let abort = task.abort_handle();
    let sign_in = BrowserSignIn::new(task, None);
    let page = page(&opened, &mut receive, &abort).await?;
    let (cancelled, _) = watch::channel(false);
    Ok(Arc::new(Pending {
        state: Mutex::new(State {
            sign_in,
            input,
            submitted: None,
        }),
        abort,
        cancelled,
        directory,
        page,
    }))
}

#[cfg(test)]
#[path = "../test/login.test.rs"]
mod tests;
