use serde_json::{json, Value};
use std::{path::PathBuf, process::Stdio};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
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
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("nora-client-profile-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(directory.join("plugin-data")).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_nora-provider-openai"))
            .env_clear()
            .env("HOME", &directory)
            .env("CODEX_HOME", &directory)
            .env("PATH", "")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        Self {
            directory,
            input: child.stdin.take().unwrap(),
            output: BufReader::new(child.stdout.take().unwrap()),
            child,
            next: 1,
        }
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
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[tokio::test]
async fn sdk_dispatches_public_profiles_without_an_account_and_preserves_model_metadata() {
    let mut host = Host::new();
    let catalog = json!({"openai":{"models":{"gpt-fixture":{"id":"gpt-fixture","cost":{"input":2,"output":8}}}}}).to_string();
    std::fs::write(
        host.directory.join("models_cache.json"),
        r#"{"client_version":"99.1.0"}"#,
    )
    .unwrap();
    let catalog_path = host.directory.join("plugin-data/models.json");
    std::fs::write(&catalog_path, &catalog).unwrap();
    let initialized = host.call("initialize", json!({
        "apiVersion":2,"pluginID":"openai","pluginDirectory":env!("CARGO_MANIFEST_DIR"),
        "dataDirectory":host.directory.join("plugin-data"),
        "manifest":serde_json::from_str::<Value>(include_str!("../nora-plugin.json")).unwrap(),
        "subscriptionAccountsEnabled":false
    })).await;
    assert_eq!(
        initialized["result"]["capabilities"]["clientProfiles"],
        json!(["codex"])
    );
    let query = json!({"models":[{"id":"gpt-fixture"}],"subscriptionAccountsEnabled":false});
    let before = host.call("modelMetadata", query.clone()).await;
    assert_eq!(
        before["result"]["models"]["gpt-fixture"]["price"]["input"],
        2.0
    );
    let profile = host
        .call(
            "modelMetadata",
            json!({"clientProfile":"codex","subscriptionAccountsEnabled":false}),
        )
        .await;
    let headers = &profile["result"]["clientProfiles"]["codex"];
    assert_eq!(headers["originator"], "codex_cli_rs");
    assert_eq!(headers["version"], "99.1.0");
    assert!(headers["user-agent"]
        .as_str()
        .unwrap()
        .starts_with("codex_cli_rs/99.1.0 "));
    assert_eq!(headers.as_object().unwrap().len(), 3);
    assert!(host
        .call("modelMetadata", json!({"clientProfile":"claude-code"}))
        .await
        .get("error")
        .is_some());
    let after = host.call("modelMetadata", query).await;
    assert_eq!(before["result"], after["result"]);
    assert_eq!(std::fs::read_to_string(catalog_path).unwrap(), catalog);
    assert!(!host.directory.join("auth.json").exists());
}
