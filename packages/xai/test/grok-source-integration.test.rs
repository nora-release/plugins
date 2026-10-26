#![cfg(unix)]
#[path = "grok_fixture.rs"]
mod fixture;
use fixture::Fixture;
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};

struct Host {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next: u64,
}

impl Host {
    async fn new(fixture: &Fixture) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_nora-provider-xai"))
            .env_clear()
            .env("HOME", &fixture.directory)
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
            child,
            input,
            output,
            next: 1,
        };
        let manifest: Value = serde_json::from_str(include_str!("../nora-provider.json")).unwrap();
        let result = host.call("initialize", json!({"apiVersion":1,"pluginID":"xai","pluginDirectory":env!("CARGO_MANIFEST_DIR"),"dataDirectory":fixture.directory.join("plugin-data"),"manifest":manifest,"subscriptionAccountsEnabled":true})).await;
        assert!(result.get("error").is_none(), "{result}");
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

    async fn refresh(&mut self, credentials: &Value, force: bool) -> Value {
        self.call(
            "refresh",
            json!({"credentials":credentials,"force":force,"networkProxy":{"mode":"direct"}}),
        )
        .await
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

#[tokio::test]
async fn compiled_plugin_retains_the_binding_after_failed_renewal_and_recovers_with_the_same_account(
) {
    let fixture = Fixture::new();
    fixture.write("private-rejected-token", "account-a", false);
    fixture.cli("echo private-diagnostic >&2; exit 1");
    let saved = fixture.saved();
    let mut host = Host::new(&fixture).await;
    let failed = host.refresh(&saved, true).await;
    assert_eq!(failed["error"]["code"], "upstream_transport_error");
    assert!(!failed.to_string().contains("private-"));
    assert!(failed.get("result").is_none());
    assert!(std::fs::read_to_string(&fixture.file)
        .unwrap()
        .contains("private-rejected-token"));
    std::fs::write(
        fixture.directory.join("replacement.json"),
        Fixture::record("private-new-token", "account-a", false).to_string(),
    )
    .unwrap();
    fixture.cli("/bin/cp \"$GROK_HOME/replacement.json\" \"$GROK_AUTH_PATH\"; exit 1");
    let recovered = host.refresh(&saved, true).await;
    assert_eq!(recovered["result"]["credentials"], saved, "{recovered}");
    assert!(!recovered.to_string().contains("private-"));
    std::fs::write(&fixture.file, "{}").unwrap();
    assert_eq!(
        host.refresh(&saved, false).await["error"]["code"],
        "authentication_required"
    );
}

#[tokio::test]
async fn compiled_plugin_retries_torn_reads_and_keeps_only_the_last_complete_same_source_record() {
    let fixture = Fixture::new();
    let mut host = Host::new(&fixture).await;
    let saved = fixture.saved();
    std::fs::write(&fixture.file, "{").unwrap();
    assert_eq!(
        host.refresh(&saved, false).await["error"]["code"],
        "upstream_transport_error"
    );
    let file = fixture.file.clone();
    let writer = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        std::fs::write(
            file,
            Fixture::record("private-complete-token", "account-a", false).to_string(),
        )
        .unwrap();
    });
    assert_eq!(
        host.refresh(&saved, false).await["result"]["credentials"],
        saved
    );
    writer.await.unwrap();
    std::fs::write(&fixture.file, "").unwrap();
    assert_eq!(
        host.refresh(&saved, false).await["result"]["credentials"],
        saved
    );
    fixture.write("private-other-token", "account-b", false);
    assert_eq!(
        host.refresh(&saved, false).await["error"]["code"],
        "authentication_required"
    );
    std::fs::write(&fixture.file, "").unwrap();
    assert_eq!(
        host.refresh(&saved, false).await["error"]["code"],
        "authentication_required"
    );
    std::fs::write(&fixture.file, "{}").unwrap();
    assert_eq!(
        host.refresh(&saved, false).await["error"]["code"],
        "authentication_required"
    );
    std::fs::write(&fixture.file, "").unwrap();
    assert_eq!(
        host.refresh(&saved, false).await["error"]["code"],
        "upstream_transport_error"
    );
}
