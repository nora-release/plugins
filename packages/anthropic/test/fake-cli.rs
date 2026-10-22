use serde_json::{json, Value};
use std::{
    io::{self, BufRead, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};
fn emit(value: Value) {
    let mut out = io::stdout().lock();
    writeln!(out, "{value}").unwrap();
    out.flush().unwrap();
}
fn event(value: Value) {
    emit(json!({"type":"stream_event","event":value}));
}
fn publish(directory: &std::path::Path, name: &str, bytes: &[u8]) {
    let temporary = directory.join(format!(".{name}.tmp"));
    std::fs::write(&temporary, bytes).unwrap();
    std::fs::rename(temporary, directory.join(name)).unwrap();
}
fn credentials(directory: &PathBuf, rotated: bool) {
    std::fs::create_dir_all(directory).unwrap();
    std::fs::write(directory.join(".credentials.json"),json!({"claudeAiOauth":{"accessToken":if rotated{"next-access"}else{"fixture-access"},"refreshToken":if rotated{"next-refresh"}else{"fixture-refresh"},"expiresAt":4102444800000i64,"scopes":["user:inference","user:profile"],"subscriptionType":"max"}}).to_string()).unwrap();
    std::fs::write(directory.join(".claude.json"),json!({"hasCompletedOnboarding":true,"oauthAccount":{"emailAddress":"person@example.test","accountUuid":"account-a","organizationUuid":"org-a","organizationName":"Example"}}).to_string()).unwrap();
}
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let directory = PathBuf::from(std::env::var_os("CLAUDE_CONFIG_DIR").expect("isolated config"));
    let proxies = [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
    ]
    .map(|key| (key, std::env::var(key).unwrap_or_default()))
    .into_iter()
    .collect::<std::collections::BTreeMap<_, _>>();
    let record = json!({"pid":std::process::id(),"args":args,"proxies":proxies,"config":directory,"cwd":std::env::current_dir().unwrap(),"builtinMCP":std::env::var("ENABLE_CLAUDEAI_MCP_SERVERS").ok(),"memory":std::env::var("CLAUDE_CODE_DISABLE_AUTO_MEMORY").ok(),"apiKey":std::env::var("ANTHROPIC_API_KEY").ok()});
    publish(
        &directory,
        &format!("fixture-process-{}.json", std::process::id()),
        &serde_json::to_vec(&record).unwrap(),
    );
    if args.starts_with(&["auth".into(), "login".into()]) {
        let address="https://claude.ai/oauth/authorize?state=fixture-state&code_challenge=fixture-challenge";
        if let Ok(browser) = std::env::var("BROWSER") {
            let _ = Command::new(browser).arg(address).status();
        }
        println!("{address}");
        io::stdout().flush().unwrap();
        if directory.join("wait-for-code").exists() {
            let mut line = String::new();
            io::stdin().read_line(&mut line).unwrap();
            if line.trim() != "fixture-code#state" {
                std::process::exit(1);
            }
        }
        credentials(&directory, false);
        return;
    }
    if args.starts_with(&["auth".into(), "status".into()]) {
        let profile: Value = std::fs::read(directory.join(".claude.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_else(|| json!({}));
        let oauth = &profile["oauthAccount"];
        emit(
            json!({"loggedIn":true,"authMethod":"claude.ai","email":oauth["emailAddress"],"accountUuid":oauth["accountUuid"],"orgUuid":oauth["organizationUuid"],"orgName":oauth["organizationName"],"subscriptionType":"max"}),
        );
        return;
    }
    if args.iter().any(|arg| arg == "/usage") {
        emit(
            json!({"type":"result","is_error":false,"result":"Current session: 25% used · resets Oct 23 at 3pm (Asia/Shanghai)\nCurrent week (all models): 40% used · resets Oct 27, 2:59pm (Asia/Shanghai)\nCurrent week (Opus): 50% used"}),
        );
        return;
    }
    let config = args
        .windows(2)
        .find(|pair| pair[0] == "--mcp-config")
        .map(|pair| serde_json::from_str::<Value>(&pair[1]).unwrap())
        .unwrap();
    let server = &config["mcpServers"]["nora"];
    let mut helper = Command::new(server["command"].as_str().unwrap())
        .args(
            server["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|arg| arg.as_str().unwrap()),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut helper_in = helper.stdin.take().unwrap();
    let helper_out = helper.stdout.take().unwrap();
    let (from_cli, to_cli) = (mpsc::channel::<Value>(), mpsc::channel::<Value>());
    let sender = from_cli.0.clone();
    thread::spawn(move || {
        for line in io::BufReader::new(helper_out).lines().flatten() {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                if sender.send(value).is_err() {
                    return;
                }
            }
        }
    });
    let sender = to_cli.0.clone();
    thread::spawn(move || {
        for line in io::stdin().lock().lines().flatten() {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                if sender.send(value).is_err() {
                    return;
                }
            }
        }
    });
    fn rpc(input: &mut impl Write, value: Value) {
        writeln!(input, "{value}").unwrap();
        input.flush().unwrap();
    }
    fn get(receiver: &mpsc::Receiver<Value>, id: u64) -> Value {
        loop {
            let value = receiver.recv().unwrap();
            if value["id"] == id {
                return value;
            }
        }
    }
    rpc(
        &mut helper_in,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{}}}),
    );
    let _ = get(&from_cli.1, 1);
    rpc(
        &mut helper_in,
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    );
    rpc(
        &mut helper_in,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    );
    let tools = get(&from_cli.1, 2)["result"]["tools"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut sequence = 0;
    while let Ok(request) = to_cli.1.recv() {
        if request["type"] == "control_response" {
            continue;
        }
        if request["type"] != "user" {
            continue;
        }
        sequence += 1;
        let prompt = request["message"]["content"].to_string();
        if prompt.contains("EARLY_LIMIT") {
            emit(
                json!({"type":"result","is_error":true,"api_error_status":429,"result":"private-access quota exhausted"}),
            );
            continue;
        }
        if prompt.contains("WAIT_CANCEL") {
            publish(
                &directory,
                &format!("fixture-waiting-{}.json", std::process::id()),
                &serde_json::to_vec(&json!({"pid":std::process::id(),"helper":helper.id()}))
                    .unwrap(),
            );
            loop {
                thread::sleep(Duration::from_secs(1));
            }
        }
        if prompt.contains("ROTATE") {
            credentials(&directory, true);
        }
        let id = format!("msg-{sequence}");
        event(
            json!({"type":"message_start","message":{"id":id,"type":"message","role":"assistant","model":"claude-test","content":[],"usage":{"input_tokens":10,"output_tokens":1,"cache_read_input_tokens":100,"cache_creation_input_tokens":20}}}),
        );
        let use_tool = sequence == 1 && prompt.contains("USE_TOOL") && !tools.is_empty();
        if use_tool {
            let wire = tools[0]["name"].as_str().unwrap();
            event(
                json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call-1","name":format!("mcp__nora__{wire}"),"input":{}}}),
            );
            event(
                json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"q\":\"test\"}"}}),
            );
            event(json!({"type":"content_block_stop","index":0}));
            event(
                json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":2}}),
            );
            event(json!({"type":"message_stop"}));
            rpc(
                &mut helper_in,
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":wire,"arguments":{"q":"test"},"_meta":{"claudecode/toolUseId":"call-1"}}}),
            );
            let mut words = String::new();
            let result = loop {
                if let Ok(value) = from_cli.1.try_recv() {
                    if value["id"] == 3 {
                        break value;
                    }
                }
                if let Ok(queued) = to_cli.1.try_recv() {
                    if queued["type"] == "user" {
                        words = queued["message"]["content"].to_string();
                        emit(
                            json!({"type":"command_lifecycle","command_uuid":queued["uuid"],"state":"queued"}),
                        );
                    }
                }
                thread::sleep(Duration::from_millis(2));
            };
            sequence += 1;
            event(
                json!({"type":"message_start","message":{"id":format!("msg-{sequence}"),"type":"message","role":"assistant","model":"claude-test","content":[],"usage":{"input_tokens":12,"output_tokens":1,"cache_read_input_tokens":111}}}),
            );
            event(
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            );
            event(
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":format!("tool answered: {} user words: {words}",result["result"])}}),
            );
            event(json!({"type":"content_block_stop","index":0}));
            event(
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":8}}),
            );
            event(json!({"type":"message_stop"}));
            emit(json!({"type":"result","is_error":false,"result":""}));
            continue;
        }
        event(
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        );
        event(
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":format!("answer {sequence}")}}),
        );
        if prompt.contains("LATE_ERROR") {
            emit(
                json!({"type":"result","is_error":true,"result":"private-refresh connection failed"}),
            );
            continue;
        }
        event(json!({"type":"content_block_stop","index":0}));
        event(
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}),
        );
        event(json!({"type":"message_stop"}));
        emit(json!({"type":"result","is_error":false,"result":""}));
    }
    let _ = helper.kill();
}
