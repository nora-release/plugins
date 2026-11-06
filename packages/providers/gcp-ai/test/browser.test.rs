use super::*;
use base64::Engine;
use std::sync::Arc;
pub(crate) fn offline_args() -> Vec<String> {
    vec![
        "--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE 127.0.0.1".into(),
        "--proxy-server=http://127.0.0.1:9".into(),
        "--proxy-bypass-list=<-loopback>".into(),
    ]
}
pub(crate) struct Directory(pub PathBuf);
impl Directory {
    pub fn new() -> Self {
        let path = std::env::temp_dir().join(format!("nora-gcp-browser-{}", uuid::Uuid::new_v4()));
        cache::directory(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub(crate) async fn offline_browser(directory: &Directory) -> Arc<Browser> {
    let profile = directory.0.join("profile");
    cache::directory(&profile).unwrap();
    let mut args = arguments(&profile, &directory.0, true, "about:blank");
    args.extend(offline_args());
    Browser::launch(&chrome_executable().unwrap(), &args)
        .await
        .unwrap()
}
pub(crate) async fn route(page: &Page, html: &str) -> tokio::task::JoinHandle<()> {
    let mut events = page.connection.subscribe();
    let connection = page.connection.clone();
    let session = page.session.clone();
    let body = base64::engine::general_purpose::STANDARD.encode(html);
    page.send("Fetch.enable", json!({"patterns":[{"urlPattern":"*"}]}))
        .await
        .unwrap();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            if event["method"] != "Fetch.requestPaused" || event["sessionId"] != session {
                continue;
            }
            let params = &event["params"];
            let response = json!({"requestId":params["requestId"],"responseCode":200,"responseHeaders":[{"name":"content-type","value":"text/html; charset=utf-8"}],"body":body});
            let _ = connection
                .send("Fetch.fulfillRequest", response, Some(&session))
                .await;
        }
    })
}
#[test]
fn signin_has_no_debug_transport_or_automation_flags_and_reuses_credit_profile() {
    let args = arguments(
        Path::new("/private/profile"),
        Path::new("/private/downloads"),
        false,
        "https://accounts.google.com/AddSession",
    );
    assert!(args
        .iter()
        .any(|arg| arg == "--user-data-dir=/private/profile"));
    assert!(!args.iter().any(|arg| arg.contains("remote-debugging")
        || arg.contains("automation")
        || arg.contains("headless")));
    assert_eq!(
        args.last().unwrap(),
        "https://accounts.google.com/AddSession"
    );
}
#[tokio::test]
async fn real_chrome_pipe_reads_dom_and_closes_its_own_process() {
    if chrome_executable().is_err() {
        eprintln!("Chrome not installed; real pipe test unavailable.");
        return;
    }
    let directory = Directory::new();
    let browser = offline_browser(&directory).await;
    let page = browser.new_page().await.unwrap();
    page.goto("data:text/html,<h1>offline</h1>").await.unwrap();
    assert_eq!(
        page.evaluate("document.querySelector('h1').innerText")
            .await
            .unwrap(),
        "offline"
    );
    assert!(page
        .evaluate("throw new Error('broken')")
        .await
        .unwrap_err()
        .message
        .contains("broken"));
    page.close().await;
    assert!(page.closed());
    browser.close().await;
    assert!(browser.connection.closed());
}
#[tokio::test]
async fn real_console_session_observes_passive_redirect_and_rejects_login_and_denied_pages() {
    if chrome_executable().is_err() {
        return;
    }
    let directory = Directory::new();
    let browser = offline_browser(&directory).await;
    let page = browser.new_page().await.unwrap();
    let routing=route(&page,"<!doctype html><h1>Google Cloud Console</h1><script>var a={\"oPEP7c\":\"me@example.com\"};</script>").await;
    page.goto("https://accounts.google.com/passive")
        .await
        .unwrap();
    page.evaluate("document.body.innerHTML='<h1>Sign in with your Google Account</h1><input name=identifier>'; setTimeout(()=>location.href='https://console.cloud.google.com/?project=example-project',150); true").await.unwrap();
    let started = tokio::time::Instant::now();
    assert_console(&page, Duration::from_secs(3)).await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(100));
    page.goto("https://console.cloud.google.com/?project=example-project")
        .await
        .unwrap();
    assert_console(&page, Duration::from_secs(2)).await.unwrap();
    assert_eq!(
        account(&page, Duration::from_secs(1)).await.unwrap(),
        "me@example.com"
    );
    assert_eq!(
        read_console(&page).await.unwrap()["project"],
        "example-project"
    );
    routing.abort();
    page.send("Fetch.disable", json!({})).await.unwrap();
    let routing = route(&page, "<h1>You need additional access</h1>").await;
    page.goto("https://console.cloud.google.com/denied")
        .await
        .unwrap();
    assert!(assert_console(&page, Duration::from_secs(1))
        .await
        .unwrap_err()
        .message
        .contains("permission denied"));
    routing.abort();
    page.send("Fetch.disable", json!({})).await.unwrap();
    let routing = route(
        &page,
        "<h1>Sign in with your Google Account</h1><input type=password>",
    )
    .await;
    page.goto("https://accounts.google.com/signin")
        .await
        .unwrap();
    assert!(assert_console(&page, Duration::from_millis(150))
        .await
        .is_err());
    assert!(assert_console(&page, Duration::ZERO)
        .await
        .unwrap_err()
        .message
        .contains("positive"));
    page.close().await;
    assert!(assert_console(&page, Duration::from_secs(1))
        .await
        .unwrap_err()
        .message
        .contains("closed"));
    routing.abort();
    browser.close().await;
}
#[tokio::test]
async fn profile_pool_reuses_browser_serializes_tabs_and_releases_for_a_waiter() {
    if chrome_executable().is_err() {
        return;
    }
    let directory = Directory::new();
    let profile = directory.0.join("pool-profile");
    let service = BrowserService {
        launch_args: Arc::new(offline_args()),
        ..BrowserService::default()
    };
    let first = service
        .with_page(&profile, |page| async move { Ok(page.connection.clone()) })
        .await
        .unwrap();
    let second = service
        .with_page(&profile, |page| async move { Ok(page.connection.clone()) })
        .await
        .unwrap();
    assert!(first.same_connection(&second));
    let waiter = cache::Lock::take(&profile_lock(&profile), Duration::from_secs(6))
        .await
        .unwrap();
    assert!(first.closed());
    drop(waiter);
    service.close().await;
}
#[tokio::test]
async fn closing_or_dropping_signin_window_terminates_owned_child_and_releases_lock() {
    let directory = Directory::new();
    let lock = cache::Lock::take(&directory.0.join("window"), Duration::from_secs(1))
        .await
        .unwrap();
    let child = tokio::process::Command::new("/bin/sleep")
        .arg("30")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut window = Window {
        child: Some(child),
        lock: Some(lock),
        downloads: None,
    };
    window.close().await;
    let _again = cache::Lock::take(&directory.0.join("window"), Duration::from_secs(1))
        .await
        .unwrap();
    assert!(window.child.is_none());
}
