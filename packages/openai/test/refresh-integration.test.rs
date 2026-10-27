use base64::Engine;
use serde_json::{json, Value};
use std::{path::PathBuf, process::Stdio};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    process::{Child, ChildStdin, ChildStdout, Command},
};

struct Host {
    directory: PathBuf,
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next: u64,
}

impl Host {
    async fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("nora-openai-refresh-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_nora-provider-openai"))
            .env_clear()
            .env("HOME", &directory)
            .env("CODEX_HOME", directory.join("codex"))
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
            child,
            input,
            output,
            next: 1,
        };
        let manifest: Value = serde_json::from_str(include_str!("../nora-provider.json")).unwrap();
        let initialized = host.call("initialize", json!({"apiVersion":1,"pluginID":"openai","pluginDirectory":env!("CARGO_MANIFEST_DIR"),"dataDirectory":host.directory.join("plugin-data"),"manifest":manifest,"subscriptionAccountsEnabled":true})).await;
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
            std::time::Duration::from_secs(5),
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

    fn source(&self, marker: &str) -> (PathBuf, Value) {
        let file = self.directory.join("source-auth.json");
        let claims = json!({"sub":"user-a","exp":4070908800_i64,"nonce":marker,"https://api.openai.com/auth":{"chatgpt_account_id":"workspace-a"}});
        let token = format!(
            "header.{}.signature",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        std::fs::write(&file, json!({"auth_mode":"chatgpt","tokens":{"account_id":"workspace-a","access_token":token,"refresh_token":"private-source-refresh"}}).to_string()).unwrap();
        let credentials = nora_provider_auth::codex_source::bind(&file).unwrap();
        (
            file,
            nora_provider::auth::encode(&credentials, "openai-codex", true),
        )
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

// The explicit loopback proxy never opens a connection to the requested host.
async fn failed_proxy(status: u16) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            socket.read_exact(&mut byte).await.unwrap();
            request.push(byte[0]);
            assert!(request.len() < 8192);
        }
        let request = String::from_utf8(request).unwrap();
        assert!(request.starts_with("CONNECT auth.openai.com:443 HTTP/1.1"));
        assert!(!request.contains("private-"));
        if status != 0 {
            socket.write_all(format!("HTTP/1.1 {status} Proxy failure\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        }
    });
    (format!("http://{address}"), task)
}

#[tokio::test]
async fn compiled_owned_refresh_does_not_mark_proxy_or_connection_failures_as_signed_out() {
    let mut host = Host::new().await;
    let saved = json!({"type":"oauth","provider":"openAICodex","credentialProvider":"openAICodex","access":"private-valid-access","refresh":"private-refresh","expires":4070908800000_i64,"extras":{"accountId":"workspace-a"}});
    for status in [400, 401, 403, 429, 503, 0] {
        let (proxy, request) = failed_proxy(status).await;
        let failed = host.refresh(&saved, true, &proxy).await;
        assert_eq!(
            failed["error"]["code"], "upstream_transport_error",
            "{failed}"
        );
        assert!(!failed.to_string().contains("private-"));
        assert!(failed.get("result").is_none());
        request.await.unwrap();
    }
    let mut missing = saved.clone();
    missing["refresh"] = Value::Null;
    let invalid = host.refresh(&missing, true, "http://127.0.0.1:1").await;
    assert_eq!(invalid["error"]["code"], "authentication_required");
    let (proxy, request) = failed_proxy(503).await;
    assert_eq!(
        host.refresh(&saved, true, &proxy).await["error"]["code"],
        "upstream_transport_error"
    );
    request.await.unwrap();
}

#[tokio::test]
async fn compiled_source_refresh_keeps_the_file_and_recovers_without_rebinding() {
    let mut host = Host::new().await;
    let (file, saved) = host.source("first-login");
    let original = std::fs::read(&file).unwrap();
    let (proxy, request) = failed_proxy(503).await;
    let failed = host.refresh(&saved, true, &proxy).await;
    assert_eq!(failed["error"]["code"], "upstream_transport_error");
    request.await.unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), original);
    assert_eq!(
        host.refresh(&saved, false, "http://127.0.0.1:1").await["result"]["credentials"],
        saved
    );
    std::fs::write(&file, "{}").unwrap();
    assert_eq!(
        host.refresh(&saved, false, "http://127.0.0.1:1").await["error"]["code"],
        "authentication_required"
    );
    host.source("new-login");
    assert_eq!(
        host.refresh(&saved, false, "http://127.0.0.1:1").await["result"]["credentials"],
        saved
    );
}

#[path = "warmup-protocol.test.rs"]
mod warmup_protocol;
