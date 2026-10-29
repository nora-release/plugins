use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    thread,
};

const FIXTURE: &str = "NORA_TEST_AUTOCLAW_REFUSAL";

#[test]
#[ignore = "Subprocess SDK fixture, launched by the refusal protocol tests"]
fn fixture() {
    let value: Value = serde_json::from_str(&std::env::var(FIXTURE).unwrap()).unwrap();
    let site = if value["site"] == "autoclaw-cn" {
        Site::China
    } else {
        Site::International
    };
    let mut provider = Autoclaw::new(site);
    let replies = serde_json::from_value(value["replies"].clone()).unwrap();
    provider.client = Some(crate::tests::Client::new(replies));
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(nora_provider_plugin_sdk::serve(provider))
        .unwrap();
}

struct Process {
    directory: PathBuf,
    child: Child,
    input: ChildStdin,
    lines: mpsc::Receiver<String>,
    reader: Option<thread::JoinHandle<()>>,
    next: u64,
}

impl Process {
    fn new(site: Site, replies: Vec<(u16, Value)>) -> Self {
        let directory =
            std::env::temp_dir().join(format!("nora-autoclaw-refusal-{}", random_hex(16).unwrap()));
        std::fs::create_dir(&directory).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "refusal_protocol_tests::fixture",
                "--ignored",
                "--nocapture",
            ])
            .env_clear()
            .env("HOME", &directory)
            .env("USERPROFILE", &directory)
            .env("PATH", "")
            .env(
                FIXTURE,
                json!({"site":site.id(),"replies":replies}).to_string(),
            )
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
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut process = Self {
            directory,
            child,
            input,
            lines,
            reader: Some(reader),
            next: 1,
        };
        let manifest: Value = serde_json::from_str(if site == Site::China {
            include_str!("../../autoclaw-cn/nora-plugin.json")
        } else {
            include_str!("../nora-plugin.json")
        })
        .unwrap();
        let result = process.call("initialize",json!({"apiVersion":2,"pluginID":site.id(),"pluginDirectory":process.directory,"dataDirectory":process.directory.join("data"),"manifest":manifest,"subscriptionAccountsEnabled":true}));
        assert!(result[0].get("error").is_none(), "{result:?}");
        process
    }

    fn call(&mut self, method: &str, params: Value) -> Vec<Value> {
        let id = self.next;
        self.next += 1;
        writeln!(
            self.input,
            "{}",
            json!({"id":id,"method":method,"params":params})
        )
        .unwrap();
        self.input.flush().unwrap();
        let mut frames = Vec::new();
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(5))
                .expect("compiled AutoClaw SDK fixture did not answer");
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            assert!(
                value["event"] != "credentials",
                "Refused requests must not save credentials"
            );
            if value["id"] != id {
                continue;
            }
            let done = value.get("result").is_some()
                || value.get("error").is_some()
                || value["event"] == "done";
            frames.push(value);
            if done {
                return frames;
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
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn compiled_sdk_preserves_login_and_ban_reason_for_both_provider_regions() {
    for site in [Site::International, Site::China] {
        for method in ["refresh", "usage", "checkin", "checkinStatus", "fetch"] {
            let banned = json!({"code":410004,"message":"The account has been banned"});
            let mut process = Process::new(site, vec![(403, banned.clone()), (403, banned)]);
            let mut params = json!({"account":"fixture","credentials":crate::tests::credentials(site),"subscriptionAccountsEnabled":true});
            if method == "fetch" {
                params["url"] = json!(format!("{}/chat/completions", site.host()));
                params["method"] = json!("POST");
                params["body"] =
                    json!(STANDARD.encode(json!({"model":"zai_auto","messages":[]}).to_string()));
            }
            let frames = process.call(method, params.clone());
            if method == "fetch" {
                assert_eq!(frames[0]["status"], 403);
                assert_eq!(frames[0]["headers"]["x-magpie-sign-in"], "kept");
                let chunks = frames
                    .iter()
                    .filter(|v| v["event"] == "chunk")
                    .flat_map(|v| STANDARD.decode(v["data"].as_str().unwrap()).unwrap())
                    .collect::<Vec<_>>();
                let body: Value = serde_json::from_slice(&chunks).unwrap();
                assert_eq!(
                    body["error"]["message"],
                    "AutoClaw: The account has been banned"
                );
            } else {
                assert_eq!(
                    frames[0]["error"]["code"], "account_refused",
                    "{method}: {frames:?}"
                );
                assert_eq!(
                    frames[0]["error"]["message"],
                    "AutoClaw: The account has been banned"
                );
            }
            let accounts = process.call("accounts", params);
            assert_eq!(accounts[0]["result"]["accounts"][0]["configured"], true);
        }
    }
}
