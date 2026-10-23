use serde_json::{json, Value};
use std::{path::PathBuf, process::Stdio};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::{timeout, Duration},
};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "nora-kiro-protocol-{}",
            nora_ai::oauth_pkce::random_hex(12)
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn cli(&self) -> PathBuf {
        match std::env::consts::OS {
            "macos" => self
                .0
                .join("Library/Application Support/kiro-cli/data.sqlite3"),
            "windows" => self.0.join("roaming/kiro-cli/data.sqlite3"),
            _ => self.0.join("data/kiro-cli/data.sqlite3"),
        }
    }
    fn ide(&self) -> PathBuf {
        self.0.join(".aws/sso/cache/kiro-auth-token.json")
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Plugin {
    child: Child,
    input: ChildStdin,
    output: Lines<BufReader<ChildStdout>>,
    next: u64,
}
impl Plugin {
    async fn start(directory: &Directory) -> Self {
        std::fs::create_dir_all(directory.0.join("tmp")).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_nora-provider-kiro"))
            .env("HOME", &directory.0)
            .env("USERPROFILE", &directory.0)
            .env("XDG_DATA_HOME", directory.0.join("data"))
            .env("XDG_CONFIG_HOME", directory.0.join("config"))
            .env("APPDATA", directory.0.join("roaming"))
            .env("TMPDIR", directory.0.join("tmp"))
            .env("TMP", directory.0.join("tmp"))
            .env("TEMP", directory.0.join("tmp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut plugin = Self {
            child,
            input,
            output,
            next: 0,
        };
        let manifest: Value = serde_json::from_str(include_str!("../nora-provider.json")).unwrap();
        let initialized = plugin
            .call(
                "initialize",
                json!({
                    "apiVersion":1,"pluginID":"kiro","pluginDirectory":env!("CARGO_MANIFEST_DIR"),
                    "dataDirectory":directory.0.join("nora"),"manifest":manifest,
                    "hostCapabilities":{"credentialUpdates":true,"httpRequestMeter":true}
                }),
            )
            .await;
        assert_eq!(initialized["capabilities"]["accountDiscovery"], true);
        plugin
    }
    async fn call(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let frame = json!({"id":self.next,"method":method,"params":params});
        self.input
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .unwrap();
        self.input.flush().await.unwrap();
        let line = timeout(Duration::from_secs(10), self.output.next_line())
            .await
            .expect("Kiro protocol call timed out")
            .unwrap()
            .expect("Kiro exited before replying");
        let frame: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(frame["id"], self.next);
        assert!(
            frame.get("event").is_none(),
            "No HTTP request or credential write is allowed for an absent or unrenewable source"
        );
        assert!(
            frame.get("error").is_none(),
            "Kiro protocol returned an unexpected error"
        );
        assert!(
            !line.contains("protocol-private-"),
            "Private source credentials escaped in a protocol response"
        );
        frame["result"].clone()
    }
    async fn stop(mut self) {
        self.input.shutdown().await.unwrap();
        drop(self.input);
        let status = timeout(Duration::from_secs(5), self.child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success());
    }
}

#[tokio::test]
async fn compiled_plugin_discovers_in_an_isolated_home_and_reports_unrenewable_sources_without_exposing_tokens(
) {
    let directory = Directory::new();
    let mut plugin = Plugin::start(&directory).await;
    assert_eq!(plugin.call("accounts", json!({})).await, json!([]));
    assert_eq!(
        plugin.call("discoverAccounts", json!({})).await,
        json!({"accounts":[]})
    );

    let cli = directory.cli();
    let ide = directory.ide();
    std::fs::create_dir_all(cli.parent().unwrap()).unwrap();
    std::fs::create_dir_all(ide.parent().unwrap()).unwrap();
    std::fs::write(&cli, b"protocol-private-broken-database").unwrap();
    std::fs::write(
        &ide,
        json!({
            "accessToken":"protocol-private-expired-token","expiresAt":"2000-01-01T00:00:00Z",
            "authMethod":"social","region":"us-east-1"
        })
        .to_string(),
    )
    .unwrap();
    let cli_before = std::fs::read(&cli).unwrap();
    let ide_before = std::fs::read(&ide).unwrap();
    let result = plugin.call("discoverAccounts", json!({})).await;
    assert_eq!(result["accounts"], json!([]));
    let mut failed = result["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|error| error["sourceID"].as_str().unwrap())
        .collect::<Vec<_>>();
    failed.sort();
    assert_eq!(failed, ["cliSource", "ideSource"]);
    assert_eq!(std::fs::read(&cli).unwrap(), cli_before);
    assert_eq!(std::fs::read(&ide).unwrap(), ide_before);

    std::fs::remove_file(cli).unwrap();
    std::fs::write(&ide, "{}").unwrap();
    assert_eq!(
        plugin.call("discoverAccounts", json!({})).await,
        json!({"accounts":[]})
    );
    assert_eq!(plugin.call("accounts", json!({})).await, json!([]));
    plugin.stop().await;
}
