use super::*;
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    thread,
};

const FIXTURE: &str = "NORA_TEST_CLINE_PIN";

struct RecordingClient {
    inner: Arc<Client>,
    log: PathBuf,
}

#[async_trait]
impl HttpClient for RecordingClient {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        let mut output = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .unwrap();
        writeln!(output, "{}", serde_json::to_string(&request).unwrap()).unwrap();
        self.inner.stream(request).await
    }
}

#[test]
#[ignore = "Compiled SDK fixture launched with isolated, in-memory HTTP replies"]
fn fixture() {
    let value: Value = serde_json::from_str(&std::env::var(FIXTURE).unwrap()).unwrap();
    let id = if value["id"] == "clinepass" {
        "clinepass"
    } else {
        "cline"
    };
    let (_, client, store) = setup(serde_json::from_value(value["replies"].clone()).unwrap());
    let client = Arc::new(RecordingClient {
        inner: client,
        log: PathBuf::from(value["log"].as_str().unwrap()),
    });
    let plugin = Cline::with_core(Core::with_dependencies(id, client, store));
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(nora_provider_plugin_sdk::serve(plugin))
        .unwrap();
}

struct Process {
    child: Child,
    input: ChildStdin,
    lines: mpsc::Receiver<String>,
    reader: Option<thread::JoinHandle<()>>,
    directory: PathBuf,
    id: u64,
}

impl Process {
    fn new(id: &str, preference: Option<Value>, replies: Vec<(u16, Value)>) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let directory = std::env::temp_dir().join(format!(
            "nora-cline-pin-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&directory).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "provider::tests::pinning_protocol_tests::fixture",
                "--ignored",
                "--nocapture",
            ])
            .env_clear()
            .env("HOME", &directory)
            .env("USERPROFILE", &directory)
            .env("PATH", "")
            .env(
                FIXTURE,
                json!({"id":id,"replies":replies,"log":directory.join("requests.jsonl")})
                    .to_string(),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut process = Self {
            child,
            input,
            lines,
            reader: Some(reader),
            directory,
            id: 0,
        };
        let mut manifest: Value = serde_json::from_str(if id == "cline" {
            include_str!("../nora-plugin.json")
        } else {
            include_str!("../../clinepass/nora-plugin.json")
        })
        .unwrap();
        if let Some(preference) = preference {
            manifest["contributes"]["provider"]["config"]["pinUpstream"] = preference;
        }
        let initialized = process.call("initialize", json!({"apiVersion":2,"pluginID":id,"pluginDirectory":process.directory,"dataDirectory":process.directory.join("data"),"manifest":manifest,"subscriptionAccountsEnabled":true}));
        assert!(initialized[0].get("result").is_some(), "{initialized:?}");
        process
    }

    fn call(&mut self, method: &str, params: Value) -> Vec<Value> {
        self.id += 1;
        writeln!(
            self.input,
            "{}",
            json!({"id":self.id,"method":method,"params":params})
        )
        .unwrap();
        self.input.flush().unwrap();
        let mut frames = Vec::new();
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(5))
                .expect("compiled Cline SDK fixture did not answer");
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if value["id"] != self.id {
                continue;
            }
            assert_ne!(
                value["event"], "credentials",
                "Chat routing must not replace the account"
            );
            let done = value.get("result").is_some()
                || value.get("error").is_some()
                || value["event"] == "done";
            frames.push(value);
            if done {
                return frames;
            }
        }
    }

    fn requests(&self) -> Vec<HttpRequest> {
        std::fs::read_to_string(self.directory.join("requests.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
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
fn compiled_sdk_keeps_three_states_and_original_body_on_a_single_automatic_retry() {
    for id in ["cline", "clinepass"] {
        for preference in [None, Some(json!(false)), Some(json!(true))] {
            let mut process = Process::new(
                id,
                preference.clone(),
                vec![
                    (503, json!({"error":"upstream unavailable"})),
                    (
                        200,
                        json!({"_raw":"data: {}\n\ndata: [DONE]\n\n","_content_type":"text/event-stream"}),
                    ),
                ],
            );
            let original =
                "{ \"messages\" : [], \"model\" : \"cline-pass/deepseek-v4.1-flash\" }\n";
            let frames = process.call("fetch",json!({"account":"fixture","credentials":{"type":"api","key":"fixture-key"},"method":"POST","url":format!("{BASE}/chat/completions"),"body":base64::engine::general_purpose::STANDARD.encode(original),"metadata":{"sessionID":"fixture-session"}}));
            assert_eq!(frames[0]["event"], "head");
            assert_eq!(
                frames[0]["status"],
                if preference.is_none() { 200 } else { 503 }
            );
            assert_eq!(frames.last().unwrap()["event"], "done");
            let requests = process.requests();
            assert_eq!(requests.len(), if preference.is_none() { 2 } else { 1 });
            if preference == Some(json!(false)) {
                assert_eq!(requests[0].body.as_ref().unwrap(), original.as_bytes());
            } else {
                let value: Value =
                    serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
                assert_eq!(
                    value["providerOptions"],
                    json!({"gateway":{"only":["deepseek"],"has":["vision","implicit-caching"]}})
                );
            }
            if preference.is_none() {
                assert_eq!(requests[1].body.as_ref().unwrap(), original.as_bytes());
                assert_eq!(requests[0].headers, requests[1].headers);
                let bytes = frames
                    .iter()
                    .filter(|frame| frame["event"] == "chunk")
                    .flat_map(|frame| {
                        base64::engine::general_purpose::STANDARD
                            .decode(frame["data"].as_str().unwrap())
                            .unwrap()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(bytes, b"data: {}\n\ndata: [DONE]\n\n");
            }
        }
    }
}
