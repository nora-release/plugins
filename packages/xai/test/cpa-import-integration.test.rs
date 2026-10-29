#![cfg(unix)]
#[path = "cpa_fixture.rs"]
mod cpa;
#[path = "grok_fixture.rs"]
mod fixture;
use fixture::Fixture;
use serde_json::{json, Value};
use std::{os::unix::fs::PermissionsExt, process::Stdio};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};

struct Host {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next: u64,
    accept: bool,
    saved: Vec<Value>,
}
impl Host {
    async fn new(fixture: &Fixture, enabled: bool) -> (Self, Value) {
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
            accept: true,
            saved: Vec::new(),
        };
        let manifest: Value = serde_json::from_str(include_str!("../nora-plugin.json")).unwrap();
        let result=host.call("initialize",json!({"apiVersion":2,"pluginID":"xai","pluginDirectory":env!("CARGO_MANIFEST_DIR"),"dataDirectory":fixture.directory.join("plugin-data"),"manifest":manifest,"subscriptionAccountsEnabled":enabled,"hostCapabilities":{"credentialUpdates":true}})).await;
        (host, result)
    }
    async fn write(&mut self, value: Value) {
        self.input
            .write_all(format!("{value}\n").as_bytes())
            .await
            .unwrap();
        self.input.flush().await.unwrap();
    }
    async fn call(&mut self, method: &str, mut params: Value) -> Value {
        params["networkProxy"] = json!({"mode":"proxy","url":"http://127.0.0.1:9"});
        let id = self.next;
        self.next += 1;
        self.write(json!({"id":id,"method":method,"params":params}))
            .await;
        loop {
            let mut line = String::new();
            let count = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                self.output.read_line(&mut line),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(count > 0, "provider ended");
            let frame: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(frame["id"], id);
            if frame["event"] == "credentials" {
                self.saved.push(frame["credentials"].clone());
                self.write(json!({"method":"credentialsSaved","params":{"id":id,"updateID":frame["updateID"],"success":self.accept}})).await;
            } else {
                return frame;
            }
        }
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

#[tokio::test]
async fn compiled_plugin_imports_a_selected_cpa_file_without_cli_and_does_not_mutate_it() {
    let fixture = Fixture::new();
    let source = cpa::source();
    std::fs::write(&fixture.file, source.to_string()).unwrap();
    std::fs::set_permissions(&fixture.file, std::fs::Permissions::from_mode(0o400)).unwrap();
    let before = std::fs::read(&fixture.file).unwrap();
    let (mut host, initialized) = Host::new(&fixture, true).await;
    assert_eq!(
        initialized["result"]["capabilities"]["sdkFeatures"]["accountImport"],
        1
    );
    let p = cpa::params(&serde_json::from_slice(&std::fs::read(&fixture.file).unwrap()).unwrap());
    let preview = host.call("inspectAccountImport", p.clone()).await;
    assert_eq!(
        preview["result"]["entries"][0]["label"],
        "fixture@example.test"
    );
    assert!(!preview.to_string().contains("private-cpa-refresh"));
    let result = host.call("importAccount", p).await;
    assert!(result.get("error").is_none(), "{result}");
    assert_eq!(host.saved.len(), 1);
    assert_eq!(host.saved[0], result["result"]["credentials"]);
    let saved = host.saved[0].clone();
    assert_eq!(saved["access"], source["access_token"]);
    assert_eq!(saved["refresh"], source["refresh_token"]);
    assert_eq!(saved["type"], "oauth");
    assert!(saved.pointer("/extras/noraAccountSource").is_none());
    let accounts = host
        .call("accounts", json!({"credentials":saved,"account":"fixture"}))
        .await;
    assert_eq!(
        accounts["result"]["accounts"][0]["email"],
        "fixture@example.test"
    );
    assert_eq!(std::fs::read(&fixture.file).unwrap(), before);
    assert!(!fixture.directory.join(".grok").exists());
    assert!(!fixture.executable.exists());
}

#[tokio::test]
async fn compiled_plugin_keeps_accounts_separate_and_requires_a_saved_import() {
    let fixture = Fixture::new();
    let (mut host, _) = Host::new(&fixture, true).await;
    let mut first = cpa::source();
    first["expired"] = json!("2000-01-01T00:00:00Z");
    let first = host.call("importAccount", cpa::params(&first)).await;
    assert_eq!(first["result"]["credentials"]["expires"], 946684800000_i64);
    let mut second = cpa::source();
    second["sub"] = json!("other-person");
    second["refresh_token"] = json!("other-private-refresh");
    let second = host.call("importAccount", cpa::params(&second)).await;
    assert_ne!(
        first["result"]["accountIdentity"],
        second["result"]["accountIdentity"]
    );
    assert_eq!(host.saved.len(), 2);
    host.accept = false;
    let refused = host
        .call("importAccount", cpa::params(&cpa::source()))
        .await;
    assert_eq!(refused["error"]["code"], "credential_persistence");
    assert!(refused.get("result").is_none());
    assert!(!refused.to_string().contains("private-cpa-refresh"));
}

#[tokio::test]
async fn compiled_plugin_rejects_invalid_cpa_files_without_saving_and_honors_subscription_setting()
{
    let fixture = Fixture::new();
    let (mut host, _) = Host::new(&fixture, true).await;
    let mut bad = cpa::source();
    bad["disabled"] = json!(true);
    let preview = host.call("inspectAccountImport", cpa::params(&bad)).await;
    assert!(
        preview["result"]["entries"][0]["error"].is_string(),
        "{preview}"
    );
    let refused = host.call("importAccount", cpa::params(&bad)).await;
    assert_eq!(refused["error"]["code"], "invalid_account_import");
    assert!(host.saved.is_empty());
    let (mut disabled, _) = Host::new(&fixture, false).await;
    let refused = disabled
        .call("importAccount", cpa::params(&cpa::source()))
        .await;
    assert_eq!(refused["error"]["code"], "subscription_accounts_disabled");
    assert!(disabled.saved.is_empty());
}
