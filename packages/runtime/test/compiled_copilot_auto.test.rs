use super::*;
use base64::Engine;
use std::{
    collections::BTreeMap,
    io::Read,
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

struct Mock {
    url: String,
    requests: Arc<Mutex<Vec<(String, BTreeMap<String, String>, Value)>>>,
    stop: Arc<AtomicBool>,
    server: Option<thread::JoinHandle<()>>,
}
impl Mock {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let received = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let server = thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                let (socket, _) = match listener.accept() {
                    Ok(socket) => socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("Mock server failed: {error}"),
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(socket);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .split('?')
                    .next()
                    .unwrap()
                    .to_string();
                let mut headers = BTreeMap::new();
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    let (key, value) = line.trim().split_once(':').unwrap();
                    headers.insert(key.to_ascii_lowercase(), value.trim().to_string());
                }
                let length = headers
                    .get("content-length")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                assert!(length < 1024 * 1024);
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let body = if body.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice::<Value>(&body).unwrap()
                };
                received.lock().unwrap().push((path.clone(), headers, body));
                let response = match path.as_str() {
                    "/models" => {
                        json!({"data":[{"id":"fixture-picked-model", "model_picker_enabled":true,
                        "policy":{"state":"enabled"}, "capabilities":{"type":"chat"}, "supported_endpoints":["/responses"]}]})
                    }
                    "/auto" => {
                        json!({"session_token":"private-fixture-auto", "selected_model":{"id":"fixture-picked-model", "supported_endpoints":["/responses"]}, "expires_at":4102444800_i64})
                    }
                    "/responses" => json!({"id":"fixture-response", "output":[]}),
                    _ => panic!("Unexpected local provider path: {path}"),
                };
                let bytes = serde_json::to_vec(&response).unwrap();
                write!(reader.get_mut(), "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).unwrap();
                reader.get_mut().write_all(&bytes).unwrap();
                reader.get_mut().flush().unwrap();
            }
        });
        Self {
            url,
            requests,
            stop,
            server: Some(server),
        }
    }
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}

#[test]
#[ignore = "Requires NORA_PROVIDER_CAPABILITY_TEST_PACKAGES with the explicitly rebuilt Copilot package"]
fn compiled_copilot_auto_selects_a_protocol_and_sends_its_private_session() {
    let package = package_root().join("github-copilot");
    let temporary = Directory::new();
    let server = Mock::new();
    let mut raw = manifest(&package);
    assert_eq!(raw["config"]["sdkCapabilities"]["modelResolution"], 1);
    raw["baseURL"] = json!(server.url);
    raw["config"]["networkProxy"] = json!({"mode":"direct"});
    let mut process = Process::new(&package, &temporary.0.join("home"), true);
    let initialized = process.call(1, "initialize", json!({"apiVersion":2,"pluginID":"github-copilot", "subscriptionAccountsEnabled":true,
        "pluginDirectory":package,"dataDirectory":temporary.0.join("data"),"manifest":raw,"config":{}}));
    assert_eq!(
        initialized["result"]["capabilities"]["sdkFeatures"]["modelResolution"],
        1
    );
    let credentials = json!({"type":"oauth", "credentialProvider":"gitHubCopilot", "access":"private-fixture-chat", "refresh":"private-fixture-github", "expires":4102444800000_i64});
    let context = json!({"account":"fixture-account", "credentials":credentials});
    let models = process.call(2, "models", context.clone());
    let auto = models["result"]["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["id"] == "auto")
        .unwrap();
    assert!(!auto.to_string().contains("private-"));
    let mut params = context.clone();
    params["model"] = auto.clone();
    params["prompt"] = json!("Respond with a greeting.");
    let resolved = process.call(3, "resolveModel", params);
    assert_eq!(
        resolved["result"]["model"]["api"],
        "github-copilot-openai-responses"
    );
    assert_eq!(resolved["result"]["model"]["id"], "fixture-picked-model");
    assert_eq!(
        resolved["result"]["headers"]["copilot-session-token"],
        "private-fixture-auto"
    );
    let body = json!({"model":"fixture-picked-model", "input":[{"role":"user", "content":[{"type":"input_text","text":"Respond with a greeting."}]}],"stream":false});
    let mut fetch = context;
    fetch["url"] = json!(format!("{}/v1/responses", server.url));
    fetch["method"] = json!("POST");
    fetch["headers"] = resolved["result"]["headers"].clone();
    fetch["body"] =
        json!(base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&body).unwrap()));
    let events = process.fetch(4, fetch);
    assert!(
        events
            .iter()
            .any(|event| event["event"] == "head" && event["status"] == 200),
        "{events:?}"
    );
    assert_eq!(events.last().unwrap()["event"], "done");
    let requests = server.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .map(|(path, _, _)| path.as_str())
            .collect::<Vec<_>>(),
        ["/models", "/auto", "/models", "/responses"]
    );
    let (_, headers, sent) = requests.last().unwrap();
    assert_eq!(headers["authorization"], "Bearer private-fixture-chat");
    assert_eq!(headers["copilot-session-token"], "private-fixture-auto");
    assert_eq!(headers["x-github-api-version"], "2026-01-09");
    assert_eq!(*sent, body);
    assert!(sent.get("messages").is_none());
}
