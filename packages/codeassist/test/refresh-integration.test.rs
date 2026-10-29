use serde_json::{json, Value};
use std::{path::PathBuf, process::Stdio, sync::Arc};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    process::{Child, ChildStdin, ChildStdout, Command},
};

static NEXT_DIRECTORY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct Host {
    directory: PathBuf,
    app: &'static str,
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next: u64,
}

impl Host {
    async fn new() -> Self {
        let app = env!("CARGO_PKG_NAME")
            .strip_prefix("nora-provider-")
            .unwrap();
        let directory = std::env::temp_dir().join(format!(
            "nora-{app}-refresh-{}-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let binary = option_env!("CARGO_BIN_EXE_nora-provider-gemini-cli")
            .or(option_env!("CARGO_BIN_EXE_nora-provider-antigravity"))
            .unwrap();
        let mut child = Command::new(binary)
            .env_clear()
            .env("HOME", &directory)
            .env("USERPROFILE", &directory)
            .env("XDG_CONFIG_HOME", directory.join("config"))
            .env("PATH", "")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut host = Self {
            directory,
            app,
            child,
            input,
            output,
            next: 1,
        };
        let manifest: Value = serde_json::from_str(match app {
            "gemini-cli" => include_str!("../../gemini-cli/nora-plugin.json"),
            "antigravity" => include_str!("../../antigravity/nora-plugin.json"),
            _ => panic!("unexpected provider"),
        })
        .unwrap();
        let initialized = host.call("initialize", json!({"apiVersion":2,"pluginID":app,"pluginDirectory":env!("CARGO_MANIFEST_DIR"),"dataDirectory":host.directory.join("plugin-data"),"manifest":manifest,"subscriptionAccountsEnabled":true})).await;
        assert!(initialized.get("error").is_none(), "{initialized}");
        host
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.input
            .write_all(format!("{}\n", json!({"id":id,"method":method,"params":params})).as_bytes())
            .await
            .unwrap();
        self.input.flush().await.unwrap();
        let mut line = String::new();
        let count = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            self.output.read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(count > 0);
        let frame: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(frame["id"], id);
        frame
    }

    async fn refresh(&mut self, saved: &Value, force: bool, proxy: &str) -> Value {
        self.call(
            "refresh",
            json!({"credentials":saved,"force":force,"networkProxy":{"mode":"proxy","url":proxy}}),
        )
        .await
    }

    fn owned(&self) -> Value {
        json!({"type":"oauth","app":self.app,"access":"private-valid-access","refresh":"private-refresh","expires":4070908800000_i64,"accountId":"bound-person","email":"person@example.test","accountIdentity":{"issuer":"https://accounts.google.com","subject":"person-a"}})
    }

    fn source(&self, native: bool) -> (PathBuf, Value) {
        let file = self.directory.join("oauth_creds.json");
        std::fs::write(&file, json!({"type":self.app,"access_token":"private-old-access","refresh_token":"private-old-refresh","expiry_date":1}).to_string()).unwrap();
        let mut saved = json!({"type":"codeAssistSource","app":self.app,"accountId":"bound-person","source":{"app":self.app,"path":file,"uid":"bound-person"}});
        if native {
            saved["source"]["kind"] = json!("geminiCLI");
        }
        (file, saved)
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

struct Proxy {
    address: String,
    requests: Arc<std::sync::Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Proxy {
    // CONNECT is refused locally; the fixture never contacts the remote host.
    async fn new(status: u16) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    socket.read_exact(&mut byte).await.unwrap();
                    request.push(byte[0]);
                    assert!(request.len() < 8192);
                }
                let request = String::from_utf8(request).unwrap();
                assert!(request.starts_with("CONNECT oauth2.googleapis.com:443 HTTP/1.1")
                    || request.starts_with("CONNECT antigravity-hub-auto-updater-974169037036.us-central1.run.app:443 HTTP/1.1"));
                assert!(!request.contains("private-"));
                seen.lock().unwrap().push(request);
                if status != 0 {
                    socket.write_all(format!("HTTP/1.1 {status} Proxy failure\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
                }
            }
        });
        Self {
            address,
            requests,
            task,
        }
    }

    fn token_requests(&self) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.starts_with("CONNECT oauth2.googleapis.com:443"))
            .count()
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn compiled_owned_refresh_distinguishes_connection_failure_from_missing_login() {
    let mut host = Host::new().await;
    let saved = host.owned();
    for status in [400, 401, 403, 429, 503, 0] {
        let proxy = Proxy::new(status).await;
        let error = host.refresh(&saved, true, &proxy.address).await;
        assert_eq!(
            error["error"]["code"], "upstream_transport_error",
            "{error}"
        );
        assert!(!error.to_string().contains("private-"));
        assert!(error.get("result").is_none());
        assert_eq!(proxy.token_requests(), 1);
        assert_eq!(
            host.refresh(&saved, false, &proxy.address).await["result"]["credentials"]["access"],
            saved["access"]
        );
        assert_eq!(proxy.token_requests(), 1);
    }
    let mut missing = saved.clone();
    missing["refresh"] = Value::Null;
    assert_eq!(
        host.refresh(&missing, true, "http://127.0.0.1:1").await["error"]["code"],
        "authentication_required"
    );
    let mut new_login = saved;
    new_login["refresh"] = json!("private-new-login-refresh");
    new_login["access"] = json!("private-new-login-access");
    assert_eq!(
        host.refresh(&new_login, false, "http://127.0.0.1:1").await["result"]["credentials"]
            ["access"],
        "private-new-login-access"
    );
}

#[tokio::test]
async fn compiled_source_refresh_preserves_credentials_and_source_binding_on_network_failures() {
    let mut host = Host::new().await;
    let kinds = if host.app == "gemini-cli" {
        vec![false, true]
    } else {
        vec![false]
    };
    for native in kinds {
        let (file, saved) = host.source(native);
        let original = std::fs::read(&file).unwrap();
        for status in [400, 401, 403, 429, 503, 0] {
            let proxy = Proxy::new(status).await;
            let error = host.refresh(&saved, false, &proxy.address).await;
            assert_eq!(
                error["error"]["code"], "upstream_transport_error",
                "{error}"
            );
            assert!(!error.to_string().contains("private-"));
            assert_eq!(std::fs::read(&file).unwrap(), original);
            assert_eq!(proxy.token_requests(), 1);
        }
        std::fs::remove_file(&file).unwrap();
        assert_eq!(
            host.refresh(&saved, false, "http://127.0.0.1:1").await["error"]["code"],
            "authentication_required"
        );
        std::fs::write(&file, original).unwrap();
        let proxy = Proxy::new(503).await;
        assert_eq!(
            host.refresh(&saved, false, &proxy.address).await["error"]["code"],
            "upstream_transport_error"
        );
        assert_eq!(proxy.token_requests(), 1);
    }
}
