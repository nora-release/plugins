use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

struct Process {
    child: Child,
    input: ChildStdin,
    lines: mpsc::Receiver<String>,
    reader: Option<thread::JoinHandle<()>>,
}
impl Process {
    fn new(package: &Path, home: &Path, subscriptions: bool) -> Self {
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        let mut child = Command::new(package.join("provider"))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("CODEX_HOME", home.join(".codex"))
            .env("NORA_NO_ACCOUNTS", if subscriptions { "0" } else { "1" })
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
                .recv_timeout(Duration::from_secs(8))
                .expect("The explicitly built provider did not answer its offline RPC");
            let value: Value = serde_json::from_str(&line).unwrap();
            if value["id"] == id && value.get("event").is_none() {
                return value;
            }
        }
    }

    fn fetch(&mut self, id: u64, params: Value) -> Vec<Value> {
        writeln!(
            self.input,
            "{}",
            json!({"id":id,"method":"fetch","params":params})
        )
        .unwrap();
        self.input.flush().unwrap();
        let mut events = Vec::new();
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(8))
                .expect("The compiled provider fetch did not finish");
            let event: Value = serde_json::from_str(&line).unwrap();
            if event["id"] != id {
                continue;
            }
            let done = event["event"] == "done" || event.get("error").is_some();
            events.push(event);
            if done {
                return events;
            }
        }
    }
}

#[path = "compiled_copilot_auto.test.rs"]
mod copilot_auto_rpc;
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
impl Directory {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "nora-compiled-capabilities-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn package_root() -> PathBuf {
    PathBuf::from(
        std::env::var_os("NORA_PROVIDER_CAPABILITY_TEST_PACKAGES").expect(
            "Set NORA_PROVIDER_CAPABILITY_TEST_PACKAGES to explicitly built isolated packages",
        ),
    )
}
fn manifest(package: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(package.join("nora-provider.json")).unwrap()).unwrap()
}
fn initialize(
    process: &mut Process,
    package: &Path,
    data: &Path,
    mut manifest: Value,
    subscriptions: bool,
) -> Value {
    // A regression can only reach a closed local proxy, never a vendor or a
    // real user account. Home, caches and data directories are isolated too.
    manifest["config"]["networkProxy"] = json!({"mode":"proxy","url":"http://127.0.0.1:1"});
    process.call(1,"initialize",json!({"apiVersion":1,"pluginID":manifest["id"],"subscriptionAccountsEnabled":subscriptions,"pluginDirectory":package,"dataDirectory":data,"manifest":manifest,"config":{}}))
}

#[test]
#[ignore = "Requires NORA_PROVIDER_CAPABILITY_TEST_PACKAGES with all explicitly rebuilt native packages"]
fn every_prepared_package_independently_confirms_the_sdk_features_it_declares() {
    let root = package_root();
    let temporary = Directory::new();
    let mut packages: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.join("nora-provider.json").is_file())
        .collect();
    packages.sort();
    assert!(packages.len() >= 93);
    for package in packages {
        let manifest = manifest(&package);
        let id = manifest["id"].as_str().unwrap();
        let mut process = Process::new(&package, &temporary.0.join(id).join("home"), false);
        let initialized = initialize(
            &mut process,
            &package,
            &temporary.0.join(id).join("data"),
            manifest.clone(),
            false,
        );
        assert_eq!(
            initialized["result"]["apiVersion"], 1,
            "{id}: {initialized}"
        );
        let declared = manifest
            .pointer("/config/sdkCapabilities")
            .and_then(Value::as_object)
            .expect("Build-time SDK declaration");
        let actual = &initialized["result"]["capabilities"]["sdkFeatures"];
        for (feature, version) in declared {
            assert_eq!(version, 1, "{id}/{feature}");
            assert_eq!(actual[feature], 1, "{id}/{feature}: {initialized}");
        }
        assert_eq!(actual["subscriptionPolicy"], 1, "{id}");
        assert_eq!(actual["automaticProxy"], 1, "{id}");
    }
    let package = root.join("compatible");
    let mut forged = manifest(&package);
    forged["config"]["accountAutomation"] = json!({"version":1});
    forged["config"]["sdkCapabilities"]["accountAutomation"] = json!(1);
    let mut process = Process::new(&package, &temporary.0.join("forged-home"), false);
    let initialized = initialize(
        &mut process,
        &package,
        &temporary.0.join("forged-data"),
        forged.clone(),
        false,
    );
    assert_eq!(initialized["result"]["apiVersion"], 1);
    assert!(
        initialized["result"]["capabilities"]["sdkFeatures"]
            .get("accountAutomation")
            .is_none(),
        "A forged manifest cannot make the generic binary implement account automation"
    );
}

fn seed_catalog(data: &Path) {
    std::fs::create_dir_all(data).unwrap();
    let models = json!({"openai":{"id":"openai","models":{"gpt-6-sol":{"id":"gpt-6-sol","modalities":{"input":["text"],"output":["text"]},"limit":{"context":200000,"output":10000}},"o3":{"id":"o3","modalities":{"output":["text"]},"limit":{"context":200000,"output":10000}}}},"anthropic":{"id":"anthropic","models":{"claude-opus-5-5":{"id":"claude-opus-5-5","modalities":{"output":["text"]},"limit":{"context":200000,"output":10000}},"claude-sonnet-5":{"id":"claude-sonnet-5","modalities":{"output":["text"]},"limit":{"context":200000,"output":10000}}}}});
    std::fs::write(
        data.join("models.json"),
        serde_json::to_vec(&models).unwrap(),
    )
    .unwrap();
}
fn tiers(result: &Value, id: &str) -> Vec<String> {
    result["result"]["models"]
        .as_array()
        .expect("Models result")
        .iter()
        .find(|model| model["id"] == id)
        .unwrap_or_else(|| panic!("Missing {id}: {result}"))["info"]["serviceTiers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}
#[test]
#[ignore = "Requires NORA_PROVIDER_CAPABILITY_TEST_PACKAGES with rebuilt OpenAI and Anthropic packages"]
fn compiled_vendor_models_advertise_fast_for_the_correct_authentication_and_protocol() {
    let root = package_root();
    let temporary = Directory::new();
    for id in ["openai", "anthropic"] {
        let package = root.join(id);
        let home = temporary.0.join(id).join("home");
        let data = temporary.0.join(id).join("data");
        seed_catalog(&data);
        let mut raw = manifest(&package);
        raw["config"]["modelDiscovery"]["noList"] = json!(true);
        let mut process = Process::new(&package, &home, true);
        std::fs::write(home.join(".codex/models_cache.json"),serde_json::to_vec(&json!({"client_version":"0.159.2","models":[{"slug":"gpt-6-sol","visibility":"list","service_tiers":[{"id":"ultrafast"}]},{"slug":"gpt-6-luna","visibility":"list"}]})).unwrap()).unwrap();
        assert_eq!(
            initialize(&mut process, &package, &data, raw, true)["result"]["apiVersion"],
            1
        );
        let api=process.call(2,"models",json!({"subscriptionAccountsEnabled":true,"credentials":{"type":"apiKey","access":"private-fixture-api-key"}}));
        let good = if id == "openai" {
            "gpt-6-sol"
        } else {
            "claude-opus-5-5"
        };
        assert!(
            tiers(&api, good).iter().any(|tier| tier == "priority"),
            "{id}: {api}"
        );
        if id == "anthropic" {
            assert!(!tiers(&api, "claude-sonnet-5")
                .iter()
                .any(|tier| tier == "priority"));
        }
        let oauth=process.call(3,"models",json!({"subscriptionAccountsEnabled":true,"credentials":{"type":"oauth","access":"private-fixture-subscription","provider":if id=="openai"{"openAICodex"}else{"anthropic"},"expires":4102444800000_i64,"extras":{"accountId":"fixture-account"}}}));
        if id == "openai" {
            assert!(tiers(&oauth, "gpt-6-sol")
                .iter()
                .any(|tier| tier == "ultrafast"));
            assert!(tiers(&oauth, "gpt-6-luna")
                .iter()
                .any(|tier| tier == "priority"));
            assert!(!tiers(&oauth, "gpt-6-luna")
                .iter()
                .any(|tier| tier == "ultrafast"));
        } else {
            assert!(
                !tiers(&oauth, "claude-opus-5-5")
                    .iter()
                    .any(|tier| tier == "priority"),
                "Claude Code does not support API fast mode"
            );
        }
    }
}
