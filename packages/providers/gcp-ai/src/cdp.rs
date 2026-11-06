use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{broadcast, oneshot, Mutex as AsyncMutex},
};

fn failure(message: impl Into<String>) -> PluginError {
    PluginError::transport(message)
}
struct Inner {
    writer: AsyncMutex<tokio::net::UnixStream>,
    pending: Mutex<BTreeMap<u64, oneshot::Sender<PluginResult<Value>>>>,
    next: AtomicU64,
    closed: AtomicBool,
    events: broadcast::Sender<Value>,
}
impl Inner {
    fn stop(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        for (_, sender) in std::mem::take(&mut *self.pending.lock().unwrap()) {
            let _ = sender.send(Err(failure(
                "Chrome closed its private debugging connection.",
            )));
        }
    }
    fn accept(&self, value: Value) {
        let Some(id) = value["id"].as_u64() else {
            let _ = self.events.send(value);
            return;
        };
        if let Some(sender) = self.pending.lock().unwrap().remove(&id) {
            let result = if let Some(error) = value.get("error") {
                Err(failure(
                    error["message"]
                        .as_str()
                        .unwrap_or("Chrome refused the command.")
                        .chars()
                        .take(512)
                        .collect::<String>(),
                ))
            } else {
                Ok(value.get("result").cloned().unwrap_or_else(|| json!({})))
            };
            let _ = sender.send(result);
        }
    }
}
#[derive(Clone)]
pub(crate) struct Pipe(Arc<Inner>);
impl Pipe {
    pub fn closed(&self) -> bool {
        self.0.closed.load(Ordering::SeqCst)
    }
    #[cfg(test)]
    pub(crate) fn same_connection(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    #[cfg(test)]
    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.0.events.subscribe()
    }
    pub fn stop(&self) {
        self.0.stop();
    }
    pub async fn send(
        &self,
        method: &str,
        params: Value,
        session: Option<&str>,
    ) -> PluginResult<Value> {
        if self.closed() {
            return Err(failure("Chrome is closed."));
        }
        let id = self.0.next.fetch_add(1, Ordering::Relaxed);
        let mut message = json!({"id":id,"method":method,"params":params});
        if let Some(session) = session {
            message["sessionId"] = json!(session);
        }
        let mut bytes = serde_json::to_vec(&message)
            .map_err(|_| failure("The Chrome command could not be encoded."))?;
        bytes.push(0);
        let (sender, receiver) = oneshot::channel();
        self.0.pending.lock().unwrap().insert(id, sender);
        struct Pending<'a>(&'a Inner, u64);
        impl Drop for Pending<'_> {
            fn drop(&mut self) {
                self.0.pending.lock().unwrap().remove(&self.1);
            }
        }
        let _pending = Pending(&self.0, id);
        if self.0.writer.lock().await.write_all(&bytes).await.is_err() {
            self.0.stop();
            return Err(failure("Chrome closed the debugging connection."));
        }
        tokio::time::timeout(Duration::from_secs(30), receiver)
            .await
            .map_err(|_| failure("Chrome command timed out after 30 seconds."))?
            .map_err(|_| failure("Chrome closed the debugging connection."))?
    }
    fn new(writer: tokio::net::UnixStream, mut reader: tokio::net::UnixStream) -> Self {
        let (events, _) = broadcast::channel(256);
        let inner = Arc::new(Inner {
            writer: AsyncMutex::new(writer),
            pending: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
            closed: AtomicBool::new(false),
            events,
        });
        let weak = Arc::downgrade(&inner);
        tokio::spawn(async move {
            let mut buffer = Vec::new();
            let mut bytes = [0u8; 8192];
            loop {
                let count = match reader.read(&mut bytes).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                buffer.extend_from_slice(&bytes[..count]);
                if buffer.len() > 8 * 1024 * 1024 {
                    break;
                }
                while let Some(end) = buffer.iter().position(|b| *b == 0) {
                    let tail = buffer.split_off(end + 1);
                    buffer.pop();
                    if let Ok(message) = serde_json::from_slice(&buffer) {
                        if let Some(inner) = weak.upgrade() {
                            inner.accept(message);
                        } else {
                            return;
                        }
                    }
                    buffer = tail;
                }
            }
            if let Some(inner) = weak.upgrade() {
                inner.stop();
            }
        });
        Self(inner)
    }
}

pub(crate) struct Browser {
    pub connection: Pipe,
    child: Mutex<Option<tokio::process::Child>>,
}
impl Browser {
    pub async fn launch(executable: &std::path::Path, args: &[String]) -> PluginResult<Arc<Self>> {
        #[cfg(unix)]
        {
            use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
            fn duplicate(fd: i32) -> PluginResult<OwnedFd> {
                let duplicated = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10) };
                if duplicated < 0 {
                    return Err(failure("Chrome's private pipe could not be opened."));
                }
                Ok(unsafe { OwnedFd::from_raw_fd(duplicated) })
            }
            let (writer, commands) = std::os::unix::net::UnixStream::pair()
                .map_err(|_| failure("Chrome's private pipe could not be opened."))?;
            let (responses, reader) = std::os::unix::net::UnixStream::pair()
                .map_err(|_| failure("Chrome's private pipe could not be opened."))?;
            let commands = duplicate(commands.as_raw_fd())?;
            let responses = duplicate(responses.as_raw_fd())?;
            let command_fd = commands.as_raw_fd();
            let response_fd = responses.as_raw_fd();
            let mut command = tokio::process::Command::new(executable);
            command
                .args(args)
                .arg("--remote-debugging-pipe")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            unsafe {
                command.pre_exec(move || {
                    if libc::dup2(command_fd, 3) < 0 || libc::dup2(response_fd, 4) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let child = command.spawn().map_err(|_| {
                failure("Chrome could not be started; set VERTEX_CHROME_PATH to its executable.")
            })?;
            drop(commands);
            drop(responses);
            writer
                .set_nonblocking(true)
                .map_err(|_| failure("Chrome's private pipe could not be opened."))?;
            reader
                .set_nonblocking(true)
                .map_err(|_| failure("Chrome's private pipe could not be opened."))?;
            let connection = Pipe::new(
                tokio::net::UnixStream::from_std(writer)
                    .map_err(|_| failure("Chrome's private pipe is unavailable."))?,
                tokio::net::UnixStream::from_std(reader)
                    .map_err(|_| failure("Chrome's private pipe is unavailable."))?,
            );
            let browser = Arc::new(Self {
                connection,
                child: Mutex::new(Some(child)),
            });
            let result = tokio::time::timeout(Duration::from_secs(30), browser.first_page()).await;
            if !matches!(result, Ok(Ok(_))) {
                browser.close().await;
                return Err(failure("Chrome did not open its private page; close Chrome using this profile and retry."));
            }
            Ok(browser)
        }
        #[cfg(not(unix))]
        {
            let _ = (executable, args);
            Err(failure("The private Chrome pipe requires a Unix system."))
        }
    }
    pub async fn first_page(&self) -> PluginResult<Page> {
        loop {
            let value = self
                .connection
                .send("Target.getTargets", json!({}), None)
                .await?;
            if let Some(target) = value["targetInfos"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|target| target["type"] == "page")
            {
                return self.attach(target["targetId"].as_str().unwrap_or("")).await;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    async fn attach(&self, target: &str) -> PluginResult<Page> {
        let result = self
            .connection
            .send(
                "Target.attachToTarget",
                json!({"targetId":target,"flatten":true}),
                None,
            )
            .await?;
        let session = result["sessionId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| failure("Chrome did not attach its private page."))?;
        let page = Page {
            connection: self.connection.clone(),
            session: session.into(),
            target: target.into(),
            closed: Arc::new(AtomicBool::new(false)),
        };
        page.send("Page.enable", json!({})).await?;
        Ok(page)
    }
    pub async fn new_page(&self) -> PluginResult<Page> {
        let value = self
            .connection
            .send("Target.createTarget", json!({"url":"about:blank"}), None)
            .await?;
        self.attach(
            value["targetId"]
                .as_str()
                .ok_or_else(|| failure("Chrome did not create a page."))?,
        )
        .await
    }
    pub async fn close(&self) {
        let child = self.child.lock().unwrap().take();
        let Some(mut child) = child else {
            return;
        };
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            self.connection.send("Browser.close", json!({}), None),
        )
        .await;
        self.connection.stop();
        if tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .is_err()
        {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
        }
    }
}
impl Drop for Browser {
    fn drop(&mut self) {
        self.connection.stop();
        if let Some(child) = self.child.get_mut().unwrap().as_mut() {
            let _ = child.start_kill();
        }
    }
}
#[derive(Clone)]
pub(crate) struct Page {
    pub connection: Pipe,
    pub session: String,
    target: String,
    closed: Arc<AtomicBool>,
}
impl Page {
    pub fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst) || self.connection.closed()
    }
    pub async fn send(&self, method: &str, params: Value) -> PluginResult<Value> {
        self.connection
            .send(method, params, Some(&self.session))
            .await
    }
    pub async fn evaluate(&self, expression: &str) -> PluginResult<Value> {
        let value = self
            .send(
                "Runtime.evaluate",
                json!({"expression":expression,"returnByValue":true,"awaitPromise":true}),
            )
            .await?;
        if value.get("exceptionDetails").is_some() {
            return Err(failure(
                value
                    .pointer("/exceptionDetails/exception/description")
                    .or_else(|| value.pointer("/exceptionDetails/text"))
                    .and_then(Value::as_str)
                    .unwrap_or("Chrome could not read the page.")
                    .chars()
                    .take(512)
                    .collect::<String>(),
            ));
        }
        Ok(value
            .pointer("/result/value")
            .cloned()
            .unwrap_or(Value::Null))
    }
    pub async fn wait(&self, expression: &str, timeout: Duration) -> PluginResult<Value> {
        if timeout.is_zero() {
            return Err(PluginError::invalid("Page wait timeout must be positive."));
        }
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.closed() {
                return Err(failure(
                    "Google Cloud page closed while checking the session.",
                ));
            }
            let result = tokio::time::timeout_at(deadline, self.evaluate(expression))
                .await
                .map_err(|_| {
                    failure(format!(
                        "Timed out after {}ms waiting for the page.",
                        timeout.as_millis()
                    ))
                })?;
            match result {
                Ok(value) if truthy(&value) => return Ok(value),
                Err(error) if !context_changed(&error.message) => return Err(error),
                _ => (),
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(failure(format!(
                    "Timed out after {}ms waiting for the page.",
                    timeout.as_millis()
                )));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    pub async fn goto(&self, url: &str) -> PluginResult<()> {
        let result = self.send("Page.navigate", json!({"url":url})).await?;
        if let Some(error) = result["errorText"].as_str().filter(|s| !s.is_empty()) {
            if !error.contains("ERR_ABORTED") && !error.contains("frame was detached") {
                return Err(failure(format!("Could not open Cloud Console: {error}")));
            }
        }
        let _ = self
            .wait(
                "document.readyState && document.readyState !== 'loading'",
                Duration::from_secs(30),
            )
            .await;
        Ok(())
    }
    pub async fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self
            .connection
            .send("Target.closeTarget", json!({"targetId":self.target}), None)
            .await;
    }
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64().is_some_and(|v| v != 0.),
        Value::String(v) => !v.is_empty(),
        _ => true,
    }
}
fn context_changed(message: &str) -> bool {
    [
        "Cannot find context",
        "Execution context was destroyed",
        "Inspected target navigated",
        "Target closed",
        "not available",
        "frame was detached",
        "no execution context",
    ]
    .iter()
    .any(|part| {
        message
            .to_ascii_lowercase()
            .contains(&part.to_ascii_lowercase())
    })
}

#[cfg(test)]
#[path = "../test/cdp.test.rs"]
mod tests;
