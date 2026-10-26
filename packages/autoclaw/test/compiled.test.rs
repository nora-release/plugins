use super::*;
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    thread,
};

struct Process {
    child: Child,
    input: ChildStdin,
    lines: mpsc::Receiver<String>,
    reader: Option<thread::JoinHandle<()>>,
}
impl Process {
    fn new(path: &Path) -> Self {
        let mut child = Command::new(path)
            .env_clear()
            .env("HOME", path.parent().unwrap())
            .env("USERPROFILE", path.parent().unwrap())
            .env("PATH", "")
            .env("NORA_NO_ACCOUNTS", "0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let Ok(line) = line else {
                    break;
                };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            input,
            lines,
            reader: Some(reader),
        }
    }
    fn call(&mut self, id: u64, method: &str, params: Value) -> Value {
        writeln!(
            self.input,
            "{}",
            json!({"id":id,"method":method,"params":params})
        )
        .unwrap();
        self.input.flush().unwrap();
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(5))
                .expect("The compiled AutoClaw executable did not answer");
            let value: Value = serde_json::from_str(&line).unwrap();
            assert!(
                value["event"] != "credentials",
                "Failed reads must not change credentials"
            );
            if value["id"] == id && value.get("event").is_none() {
                return value;
            }
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
#[ignore = "Requires NORA_AUTOCLAW_TEST_PACKAGES pointing to explicitly prepared native packages"]
fn both_compiled_native_packages_serve_models_identity_and_global_policy_without_vendor_requests() {
    let root = PathBuf::from(
        std::env::var_os("NORA_AUTOCLAW_TEST_PACKAGES")
            .expect("Set NORA_AUTOCLAW_TEST_PACKAGES to the isolated prepared packages"),
    );
    let temporary = Directory(std::env::temp_dir().join(format!(
        "nora-autoclaw-compiled-{}",
        random_hex(16).unwrap()
    )));
    std::fs::create_dir_all(&temporary.0).unwrap();
    let mut identities = Vec::new();
    for site in [Site::International, Site::China] {
        let package = root.join(site.id());
        let mut manifest: Value =
            serde_json::from_slice(&std::fs::read(package.join("nora-provider.json")).unwrap())
                .unwrap();
        // Even a regression cannot contact an upstream: all HTTP is directed
        // through a deliberately closed local proxy in these private fixtures.
        manifest["config"]["networkProxy"] = json!({"mode":"proxy","url":"http://127.0.0.1:1"});
        let mut process = Process::new(&package.join("provider"));
        let initialized=process.call(1,"initialize",json!({"apiVersion":1,"pluginID":site.id(),"subscriptionAccountsEnabled":true,"pluginDirectory":package,"dataDirectory":temporary.0.join(site.id()),"manifest":manifest,"config":{}}));
        assert_eq!(initialized["result"]["apiVersion"], 1);
        assert_eq!(initialized["result"]["capabilities"]["persistent"], true);
        let credentials = crate::tests::credentials(site);
        let params = json!({"account":"fixture","subscriptionAccountsEnabled":true,"credentials":credentials});
        let models = process.call(2, "models", params.clone());
        assert_eq!(models["result"]["source"], "catalog");
        assert_eq!(models["result"]["models"].as_array().unwrap().len(), 4);
        let accounts = process.call(3, "accounts", params.clone());
        identities.push(accounts["result"]["credentials"]["accountIdentity"].clone());
        assert_eq!(identities.last().unwrap()["subject"], "real-user");
        let mut blocked = params.clone();
        blocked["subscriptionAccountsEnabled"] = json!(false);
        let denied = process.call(4, "models", blocked);
        assert_eq!(denied["error"]["code"], "subscription_accounts_disabled");
        assert_eq!(
            process.call(5, "models", params)["result"]["models"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
    }
    assert_ne!(identities[0], identities[1]);
}

#[test]
#[ignore = "Requires NORA_AUTOCLAW_TEST_PACKAGES pointing to explicitly prepared native packages"]
fn native_network_failures_keep_login_and_missing_refresh_still_requires_login() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let root = PathBuf::from(std::env::var_os("NORA_AUTOCLAW_TEST_PACKAGES").unwrap());
    for site in [Site::International, Site::China] {
        let package = root.join(site.id());
        let mut manifest: Value =
            serde_json::from_slice(&std::fs::read(package.join("nora-provider.json")).unwrap())
                .unwrap();
        manifest["config"]["networkProxy"] = json!({"mode":"proxy","url":"http://127.0.0.1:1"});
        let mut process = Process::new(&package.join("provider"));
        let initialized=process.call(1,"initialize",json!({"apiVersion":1,"pluginID":site.id(),"pluginDirectory":package,"dataDirectory":package.join("data"),"manifest":manifest,"subscriptionAccountsEnabled":true}));
        assert!(initialized.get("error").is_none());
        let credentials = crate::tests::credentials(site);
        let params = json!({"account":"fixture","credentials":credentials,"subscriptionAccountsEnabled":true});
        for (index, method) in ["refresh", "usage", "checkin", "checkinStatus", "fetch"]
            .into_iter()
            .enumerate()
        {
            let mut request = params.clone();
            if method == "fetch" {
                request["url"] = json!(format!("{}/chat/completions", site.host()));
                request["method"] = json!("POST");
                request["body"] =
                    json!(STANDARD.encode(json!({"model":"zai_auto","messages":[]}).to_string()));
            }
            let id = 2 + 2 * index as u64;
            let error = process.call(id, method, request);
            assert_eq!(
                error["error"]["code"], "upstream_transport_error",
                "{method}: {error}"
            );
            assert!(!error.to_string().contains("access-one"));
            assert!(!error.to_string().contains("refresh-one"));
            let account = process.call(id + 1, "accounts", params.clone());
            assert_eq!(account["result"]["accounts"][0]["configured"], true);
            assert_eq!(
                account["result"]["credentials"]["access"],
                credentials["access"]
            );
            assert_eq!(
                account["result"]["credentials"]["refresh"],
                credentials["refresh"]
            );
        }
        let mut missing = params;
        missing["credentials"]["refresh"] = Value::Null;
        assert_eq!(
            process.call(20, "refresh", missing)["error"]["code"],
            "authentication_required"
        );
    }
}
