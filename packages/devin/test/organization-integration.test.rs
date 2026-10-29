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
    async fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("nora-devin-organization-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_nora-provider-devin"))
            .env_clear()
            .env("HOME", &directory)
            .env("USERPROFILE", &directory)
            .env("XDG_CONFIG_HOME", directory.join("config"))
            .env("XDG_DATA_HOME", directory.join("data"))
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
        let manifest: Value = serde_json::from_str(include_str!("../nora-plugin.json")).unwrap();
        let initialized = host
            .call(
                "initialize",
                json!({
                    "apiVersion":2,"pluginID":"devin","pluginDirectory":env!("CARGO_MANIFEST_DIR"),
                    "dataDirectory":host.directory.join("plugin-data"),"manifest":manifest,
                    "subscriptionAccountsEnabled":true
                }),
            )
            .await;
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
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[tokio::test]
async fn compiled_plugin_returns_distinct_public_organization_profiles_for_one_email() {
    let mut host = Host::new().await;
    for (id, org, name) in [
        ("chosen-one", "org_1", "opt"),
        ("chosen-two", "org_2", "lab"),
    ] {
        let saved = json!({
            "type":"api","authMethod":"browser","key":format!("private-{id}"),
            "accountId":"m@x.dev","email":"m@x.dev","label":format!("m@x.dev · {name}"),
            "displayName":format!("m@x.dev · {name}"),"organizationId":org,
            "workspaceId":format!("team-{org}"),"endpoint":"https://server.codeium.com",
            "metadata":{"profileVersion":1,"email":"m@x.dev","plan":"Devin Max",
                "organization":name,"orgId":org,"teamId":format!("team-{org}"),
                "server":"https://server.codeium.com"}
        });
        let response = host
            .call(
                "accounts",
                json!({
                    "account":id,"credentials":saved,"subscriptionAccountsEnabled":true,
                    "networkProxy":{"mode":"proxy","url":"http://127.0.0.1:1"}
                }),
            )
            .await;
        assert!(response.get("error").is_none(), "{response}");
        let profile = &response["result"][0];
        assert_eq!(profile["id"], id);
        assert_eq!(profile["email"], "m@x.dev");
        assert_eq!(profile["displayName"], format!("m@x.dev · {name}"));
        assert_eq!(profile["organization"], name);
        assert_eq!(profile["orgId"], org);
        assert_eq!(profile["teamId"], format!("team-{org}"));
        assert_eq!(profile["plan"], "Devin Max");
        assert!(!response.to_string().contains("private-"));
        assert!(profile.get("source").is_none());
        assert!(profile.get("accountIdentity").is_none());
    }
}
