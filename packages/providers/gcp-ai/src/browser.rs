use crate::{
    cache,
    cdp::{Browser, Page},
};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
#[cfg(test)]
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Mutex as AsyncMutex;

fn failed(message: impl Into<String>) -> PluginError {
    PluginError::transport(message)
}
fn seconds(name: &str, fallback: f64) -> PluginResult<Duration> {
    let value = std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .map(|value| value.parse::<f64>().unwrap_or(f64::NAN))
        .unwrap_or(fallback);
    if !value.is_finite() || value < 0. {
        return Err(PluginError::invalid(format!(
            "{name} must be a nonnegative number of seconds."
        )));
    }
    Duration::try_from_secs_f64(value)
        .map_err(|_| PluginError::invalid(format!("{name} is too large.")))
}
fn root() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(".cache")
        })
        .join("nora/gcp-ai")
}
pub(crate) fn profile_lock(profile: &Path) -> PathBuf {
    root()
        .join("profiles")
        .join(&cache::digest(&profile.to_string_lossy())[..16])
}
pub(crate) fn chrome_executable() -> PluginResult<PathBuf> {
    if let Some(path) = std::env::var_os("VERTEX_CHROME_PATH").filter(|value| !value.is_empty()) {
        let mut path = PathBuf::from(path);
        if let Some(value) = path.to_str().and_then(|value| value.strip_prefix("~/")) {
            path = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(value);
        }
        return Ok(path);
    }
    let mut paths = vec![PathBuf::from(
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    )];
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(
            PathBuf::from(home).join("Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
        );
    }
    paths.extend(
        [
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/snap/bin/chromium",
        ]
        .map(PathBuf::from),
    );
    paths
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| failed("Chrome not found; set VERTEX_CHROME_PATH to its executable."))
}
fn arguments(profile: &Path, downloads: &Path, headless: bool, url: &str) -> Vec<String> {
    let mut args = vec![
        format!("--user-data-dir={}", profile.display()),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-background-networking".into(),
        "--disable-sync".into(),
        "--disable-default-apps".into(),
        "--disable-component-update".into(),
        "--disable-client-side-phishing-detection".into(),
        "--disable-domain-reliability".into(),
        "--disable-features=Translate,OptimizationHints,MediaRouter,DialMediaRouteProvider".into(),
        "--metrics-recording-only".into(),
        "--no-service-autorun".into(),
        "--window-size=1440,1000".into(),
        format!("--crash-dumps-dir={}", downloads.display()),
    ];
    if cfg!(target_os = "macos") {
        args.push("--use-mock-keychain".into());
    } else if cfg!(target_os = "linux") {
        args.push("--password-store=basic".into());
    }
    if headless {
        args.extend(["--headless=new".into(), "--disable-gpu".into()]);
    }
    if !url.is_empty() {
        args.push(url.into());
    }
    args
}
async fn claim(profile: &Path) -> PluginResult<cache::Lock> {
    cache::directory(profile)?;
    let path = profile_lock(profile);
    let lock = cache::Lock::take(&path, Duration::from_secs(180)).await?;
    let gap = seconds("VERTEX_CHROME_GAP", 0.)?;
    if !gap.is_zero() {
        if let Ok(value) = std::fs::read_to_string(path.with_extension("since")) {
            if let Ok(last) = chrono::DateTime::parse_from_rfc3339(value.trim()) {
                let wait = last.timestamp_millis() + gap.as_millis() as i64
                    - chrono::Utc::now().timestamp_millis();
                if wait > 0 {
                    tokio::time::sleep(Duration::from_millis(wait as u64)).await;
                }
            }
        }
    }
    Ok(lock)
}
struct Downloads(PathBuf);
impl Downloads {
    fn new() -> PluginResult<Self> {
        let path = std::env::temp_dir().join(format!("nora-vertex-{}", uuid::Uuid::new_v4()));
        cache::directory(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Downloads {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) struct Window {
    child: Option<tokio::process::Child>,
    lock: Option<cache::Lock>,
    downloads: Option<Downloads>,
}
impl Window {
    pub async fn wait(&mut self) -> PluginResult<()> {
        let Some(child) = self.child.as_mut() else {
            return Ok(());
        };
        let status = child
            .wait()
            .await
            .map_err(|_| failed("Chrome sign-in window stopped unexpectedly."))?;
        self.child = None;
        self.downloads = None;
        self.lock = None;
        if status.success() {
            Ok(())
        } else {
            Err(failed("Chrome sign-in window was closed."))
        }
    }
    pub async fn close(&mut self) {
        if let Some(mut child) = self.child.take() {
            terminate(&mut child).await;
        }
        self.downloads = None;
        self.lock = None;
    }
}
impl Drop for Window {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let lock = self.lock.take();
            let downloads = self.downloads.take();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    terminate(&mut child).await;
                    drop(downloads);
                    drop(lock);
                });
            } else {
                let _ = child.start_kill();
            }
        }
    }
}
async fn terminate(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
    #[cfg(not(unix))]
    let _ = child.start_kill();
    if tokio::time::timeout(Duration::from_secs(3), child.wait())
        .await
        .is_err()
    {
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    }
}
struct Held {
    browser: Arc<Browser>,
    lock: cache::Lock,
    _downloads: Downloads,
    last: tokio::time::Instant,
}
#[derive(Default)]
struct Slot {
    operation: AsyncMutex<()>,
    held: Mutex<Option<Held>>,
}
impl Drop for Slot {
    fn drop(&mut self) {
        if let Some(held) = self.held.get_mut().unwrap().take() {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    held.browser.close().await;
                    drop(held);
                });
            }
        }
    }
}
#[derive(Clone, Default)]
pub(crate) struct BrowserService {
    slots: Arc<Mutex<BTreeMap<PathBuf, Arc<Slot>>>>,
    #[cfg(test)]
    pub(crate) launch_args: Arc<Vec<String>>,
    #[cfg(test)]
    signin_executable: Option<PathBuf>,
}
impl BrowserService {
    #[cfg(test)]
    pub(crate) fn with_signin_executable(path: PathBuf) -> Self {
        Self {
            signin_executable: Some(path),
            ..Self::default()
        }
    }
    pub async fn open_signin(&self, profile: &Path, url: &str) -> PluginResult<Window> {
        // A login outranks a warm browser kept for credits reads.
        let slot = { self.slots.lock().unwrap().get(profile).cloned() };
        if let Some(slot) = slot {
            let _operation = slot.operation.lock().await;
            let held = slot.held.lock().unwrap().take();
            if let Some(held) = held {
                held.browser.close().await;
                drop(held);
            }
        }
        let lock = claim(profile).await?;
        let downloads = Downloads::new()?;
        let args = arguments(profile, &downloads.0, false, url);
        let executable = chrome_executable();
        #[cfg(test)]
        let executable = self.signin_executable.clone().map(Ok).unwrap_or(executable);
        let child = tokio::process::Command::new(executable?)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| {
                failed("Chrome could not be started; set VERTEX_CHROME_PATH to its executable.")
            })?;
        Ok(Window {
            child: Some(child),
            lock: Some(lock),
            downloads: Some(downloads),
        })
    }
    pub async fn with_page<T, F, Fut>(&self, profile: &Path, use_page: F) -> PluginResult<T>
    where
        F: FnOnce(Page) -> Fut,
        Fut: std::future::Future<Output = PluginResult<T>>,
    {
        let idle = seconds("VERTEX_CHROME_IDLE", 30.)?;
        let slot = self
            .slots
            .lock()
            .unwrap()
            .entry(profile.into())
            .or_default()
            .clone();
        let _operation = slot.operation.lock().await;
        let old = {
            let mut held = slot.held.lock().unwrap();
            if held
                .as_ref()
                .is_some_and(|held| held.browser.connection.closed())
            {
                held.take()
            } else {
                None
            }
        };
        if let Some(old) = old {
            old.browser.close().await;
            drop(old);
        }
        if slot.held.lock().unwrap().is_none() {
            let lock = claim(profile).await?;
            let downloads = Downloads::new()?;
            let args = arguments(profile, &downloads.0, true, "about:blank");
            #[cfg(test)]
            let args = {
                let mut args = args;
                args.extend(self.launch_args.iter().cloned());
                args
            };
            let browser = Browser::launch(&chrome_executable()?, &args).await?;
            *slot.held.lock().unwrap() = Some(Held {
                browser,
                lock,
                _downloads: downloads,
                last: tokio::time::Instant::now(),
            });
            let weak = Arc::downgrade(&slot);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(
                        Duration::from_secs(2).min(idle.max(Duration::from_millis(1))),
                    )
                    .await;
                    let Some(slot) = weak.upgrade() else {
                        return;
                    };
                    let Ok(_operation) = slot.operation.try_lock() else {
                        continue;
                    };
                    let close = {
                        let mut held = slot.held.lock().unwrap();
                        if held.as_ref().is_some_and(|held| {
                            held.lock.wanted()
                                || held.last.elapsed() >= idle
                                || held.browser.connection.closed()
                        }) {
                            held.take()
                        } else {
                            None
                        }
                    };
                    if let Some(held) = close {
                        held.browser.close().await;
                        drop(held);
                        return;
                    }
                    if slot.held.lock().unwrap().is_none() {
                        return;
                    }
                }
            });
        }
        let browser = slot.held.lock().unwrap().as_ref().unwrap().browser.clone();
        let page = browser.new_page().await?;
        struct Lease {
            page: Page,
            slot: Arc<Slot>,
        }
        impl Drop for Lease {
            fn drop(&mut self) {
                if let Some(held) = self.slot.held.lock().unwrap().as_mut() {
                    held.last = tokio::time::Instant::now();
                }
                let page = self.page.clone();
                tokio::spawn(async move {
                    page.close().await;
                });
            }
        }
        let lease = Lease {
            page: page.clone(),
            slot: slot.clone(),
        };
        let result = use_page(page).await;
        lease.page.close().await;
        drop(lease);
        result
    }
    #[cfg(test)]
    pub async fn close(&self) {
        let slots = self
            .slots
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for slot in slots {
            let _operation = slot.operation.lock().await;
            let held = slot.held.lock().unwrap().take();
            if let Some(held) = held {
                held.browser.close().await;
                drop(held);
            }
        }
    }
}
pub(crate) async fn assert_console(page: &Page, timeout: Duration) -> PluginResult<()> {
    if timeout.is_zero() {
        return Err(PluginError::invalid("Page wait timeout must be positive."));
    }
    let expression = r#"(() => { if(location.origin !== 'https://console.cloud.google.com' || !document.body) return false; const text=document.body.innerText; const login=/Sign in with your Google Account|Sign in to (?:your )?Google Account/i.test(text)||(document.querySelector('input[type="password"], input[name="identifier"]')&&/sign in/i.test(text)); return text.trim()&&!login ? {text} : false })()"#;
    let value = page.wait(expression, timeout).await.map_err(|error| {
        if page.closed()
            || !error
                .message
                .to_ascii_lowercase()
                .contains("timed out after")
        {
            error
        } else {
            failed("Google session expired; sign in with the Chrome profile again.")
        }
    })?;
    let text = value["text"].as_str().unwrap_or("");
    if [
        "You do not have permission",
        "Permission denied",
        "You need additional access",
    ]
    .iter()
    .any(|needle| {
        text.to_ascii_lowercase()
            .contains(&needle.to_ascii_lowercase())
    }) {
        return Err(failed(
            "Google Cloud Console permission denied for the signed-in Google account.",
        ));
    }
    Ok(())
}
pub(crate) async fn account(page: &Page, timeout: Duration) -> PluginResult<String> {
    let value=page.wait(r#"(() => {const value=(document.documentElement?.innerHTML??'').match(/"oPEP7c":"([A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,})"/)?.[1]; return value&&!/(\.gserviceaccount\.com|@system\.|noreply|no-reply)/i.test(value)?value:null;})()"#,timeout).await?;
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| failed("Cloud Console did not identify the account."))
}
#[cfg(test)]
pub(crate) async fn read_console(page: &Page) -> PluginResult<Value> {
    let project=page.wait(r#"(() => {if(location.origin !== 'https://console.cloud.google.com')return null;const valid=v=>typeof v==='string'&&/^[a-z][a-z0-9-]{4,61}[a-z0-9]$/.test(v);let stored;try{stored=sessionStorage.getItem('last_project_id')}catch{}for(const value of [new URL(location.href).searchParams.get('project'),window.cloudConsoleContext?.project?.id,stored])if(valid(value))return value;const ids=[...new Set([...document.querySelectorAll('[data-project-id]')].map(el=>el.getAttribute('data-project-id')).filter(valid))];if(ids.length===1)return ids[0];const linked=[...new Set([...document.querySelectorAll('a[href]')].flatMap(link=>{try{const u=new URL(link.href,location.href),v=u.searchParams.get('project');return u.origin===location.origin&&valid(v)?[v]:[]}catch{return[]}}))];return !ids.length&&linked.length===1?linked[0]:null;})()"#,Duration::from_secs(30)).await.unwrap_or(Value::Null);
    let account = account(page, Duration::from_secs(5)).await.ok();
    Ok(json!({"project":project,"account":account}))
}

#[cfg(test)]
#[path = "../test/browser.test.rs"]
pub(crate) mod tests;
