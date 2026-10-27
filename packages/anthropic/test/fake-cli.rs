use serde_json::{json, Value};
use std::{
    io::{self, BufRead, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};
#[path = "fake-login.rs"]
mod login_fixture;

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
fn scripted_reply(directory: &std::path::Path, name: &str) {
    let Ok(bytes) = std::fs::read(directory.join(name)) else {
        return;
    };
    let reply: Value = serde_json::from_slice(&bytes).unwrap();
    if let Some(text) = reply["stdout"].as_str() {
        print!("{text}");
    }
    if let Some(text) = reply["stderr"].as_str() {
        eprint!("{text}");
    }
    io::stdout().flush().unwrap();
    io::stderr().flush().unwrap();
    std::process::exit(reply["exit"].as_i64().unwrap_or_default() as i32);
}
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if login_fixture::keychain(&args) {
        return;
    }
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
    let record = json!({"pid":std::process::id(),"args":args,"proxies":proxies,"config":directory,"cwd":std::env::current_dir().unwrap(),"builtinMCP":std::env::var("ENABLE_CLAUDEAI_MCP_SERVERS").ok(),"memory":std::env::var("CLAUDE_CODE_DISABLE_AUTO_MEMORY").ok(),"apiKey":std::env::var("ANTHROPIC_API_KEY").ok(),"apiBase":std::env::var("ANTHROPIC_BASE_URL").ok()});
    publish(
        &directory,
        &format!("fixture-process-{}.json", std::process::id()),
        &serde_json::to_vec(&record).unwrap(),
    );
    if args.starts_with(&["auth".into(), "login".into()]) {
        if login_fixture::run(&directory) {
            return;
        }
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
        scripted_reply(&directory, "fixture-status.json");
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
        scripted_reply(&directory, "fixture-usage.json");
        emit(
            json!({"type":"result","is_error":false,"result":"Current session: 25% used · resets Oct 23 at 3pm (Asia/Shanghai)\nCurrent week (all models): 40% used · resets Oct 27, 2:59pm (Asia/Shanghai)\nCurrent week (Opus): 50% used"}),
        );
        return;
    }
    let work = std::env::current_dir().unwrap();
    let project = work
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>();
    let project = directory.join("projects").join(project);
    std::fs::create_dir_all(&project).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let resumed = args
        .windows(2)
        .find(|pair| pair[0] == "--resume")
        .map(|pair| pair[1].clone());
    let session = resumed
        .clone()
        .unwrap_or_else(|| format!("fixture-session-{}", std::process::id()));
    let session_path = project.join(format!("{session}.jsonl"));
    if resumed.is_some()
        && (!session_path.is_file() || directory.join("fixture-fail-resume").exists())
    {
        eprintln!("No conversation found with session ID");
        std::process::exit(1);
    }
    let initial_sequence = if resumed.is_some() {
        std::fs::read_to_string(&session_path)
            .unwrap()
            .lines()
            .count()
            * 100
    } else {
        0
    };
    let mut session_log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&session_path)
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        session_log
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .unwrap();
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
    emit(json!({"type":"system","subtype":"init","session_id":session}));
    let mut sequence = initial_sequence;
    while let Ok(request) = to_cli.1.recv() {
        if request["type"] == "control_request" {
            let mut log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(directory.join(format!("fixture-controls-{}.jsonl", std::process::id())))
                .unwrap();
            writeln!(log, "{request}").unwrap();
            match request["request"]["subtype"].as_str().unwrap() {
                "initialize" => {}
                "apply_flag_settings" => {
                    if directory.join("fixture-refuse-effort").exists() {
                        emit(
                            json!({"type":"control_response","response":{"subtype":"error","request_id":request["request_id"],"error":"unsupported"}}),
                        );
                        continue;
                    }
                }
                other => panic!("unexpected control request {other}"),
            }
            emit(
                json!({"type":"control_response","response":{"subtype":"success","request_id":request["request_id"],"response":{}}}),
            );
            continue;
        }
        if request["type"] == "control_response" {
            continue;
        }
        if request["type"] != "user" {
            continue;
        }
        writeln!(session_log, "{}", request["message"]["content"]).unwrap();
        session_log.flush().unwrap();
        publish(
            &directory,
            &format!("fixture-input-{}.json", std::process::id()),
            &serde_json::to_vec(&request["message"]["content"]).unwrap(),
        );
        sequence += 1;
        let prompt = request["message"]["content"].to_string();
        if let Ok(bytes)=std::fs::read(directory.join("fixture-rate-limit-event.json")) {
            if let Ok(value)=serde_json::from_slice::<Value>(&bytes) { emit(value); }
        }
        if prompt.contains("EARLY_LIMIT") {
            emit(
                json!({"type":"result","is_error":true,"api_error_status":429,"result":"private-access quota exhausted"}),
            );
            continue;
        }
        if prompt.contains("WAIT_REWIND") && !directory.join("fixture-rewound").exists() {
            event(
                json!({"type":"message_start","message":{"id":format!("interrupted-{sequence}"),"type":"message","role":"assistant","model":"claude-test","content":[],"usage":{"input_tokens":10,"output_tokens":1}}}),
            );
            event(
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            );
            event(
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"partial cancelled answer"}}),
            );
            publish(
                &directory,
                "fixture-rewind-waiting",
                request["uuid"].as_str().unwrap().as_bytes(),
            );
            loop {
                let control = to_cli.1.recv().unwrap();
                if control["type"] != "control_request" {
                    continue;
                }
                assert_eq!(control["request"]["subtype"], "rewind_conversation");
                assert_eq!(control["request"]["target_message_uuid"], request["uuid"]);
                assert_eq!(control["request"]["interrupt_if_running"], true);
                let mut log = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(directory.join(format!("fixture-controls-{}.jsonl", std::process::id())))
                    .unwrap();
                writeln!(log, "{control}").unwrap();
                let refused = directory.join("fixture-refuse-rewind").exists();
                publish(&directory, "fixture-rewound", b"true");
                emit(
                    json!({"type":"result","is_error":true,"result":"interrupted cancelled turn"}),
                );
                emit(
                    json!({"type":"control_response","response":{"subtype":if refused {"error"} else {"success"},"request_id":control["request_id"],"response":{"rewound":!refused}}}),
                );
                break;
            }
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
        if prompt.contains("TOOL_CHOICE_NONE") {
            assert!(tools.is_empty());
        }
        if prompt.contains("TOOL_CHOICE_NAMED") {
            assert_eq!(tools.len(), 1);
            assert_eq!(tools[0]["name"], "selected");
            assert!(prompt.contains("You must call the mcp__nora__selected tool."));
        }
        if prompt.contains("TOOL_CHOICE_REQUIRED") {
            assert!(prompt.contains("You must call at least one available tool before answering."));
        }
        let id = format!("msg-{sequence}");
        if prompt.contains("REFUSAL") {
            let details = json!({"type":"refusal","category":"cyber","explanation":"flagged"});
            let mut message = json!({"id":id,"type":"message","role":"assistant","model":"claude-test","content":[],"stop_reason":"refusal","stop_sequence":null,"stop_details":details,"usage":{"input_tokens":3,"output_tokens":2}});
            if prompt.contains("WHOLE_REFUSAL") {
                emit(json!({"type":"assistant","message":message}));
            } else {
                message["stop_reason"] = Value::Null;
                message.as_object_mut().unwrap().remove("stop_details");
                message["usage"]["output_tokens"] = json!(0);
                event(json!({"type":"message_start","message":message}));
                event(
                    json!({"type":"message_delta","delta":{"stop_reason":"refusal","stop_sequence":null,"stop_details":details},"usage":{"output_tokens":2}}),
                );
                event(json!({"type":"message_stop"}));
            }
            emit(json!({"type":"result","is_error":false,"result":""}));
            continue;
        }
        if args.windows(2).any(|pair| pair[0] == "--json-schema") && !prompt.contains("USE_TOOL") {
            let schema: Value = serde_json::from_str(
                &args
                    .windows(2)
                    .find(|pair| pair[0] == "--json-schema")
                    .unwrap()[1],
            )
            .unwrap();
            assert!(schema.is_object());
            let mut message = id.clone();
            event(
                json!({"type":"message_start","message":{"id":message,"type":"message","role":"assistant","model":"claude-test","content":[],"usage":{"input_tokens":10,"output_tokens":1}}}),
            );
            event(
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"not valid JSON"}}),
            );
            event(json!({"type":"content_block_stop","index":0}));
            if prompt.contains("SCHEMA_MISSING") {
                event(
                    json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
                );
                event(json!({"type":"message_stop"}));
                emit(json!({"type":"result","is_error":false,"result":"not valid JSON"}));
                continue;
            }
            if prompt.contains("SCHEMA_WORDS_FIRST") {
                event(
                    json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
                );
                event(json!({"type":"message_stop"}));
                emit(
                    json!({"type":"user","message":{"role":"user","content":[{"type":"text","text":"You MUST call the StructuredOutput tool"}]}}),
                );
                sequence += 1;
                message = format!("msg-{sequence}");
                event(
                    json!({"type":"message_start","message":{"id":message,"type":"message","role":"assistant","model":"claude-test","content":[],"usage":{"input_tokens":12,"output_tokens":1}}}),
                );
            }
            if prompt.contains("SCHEMA_RETRY") || prompt.contains("SCHEMA_GIVE_UP") {
                event(
                    json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"schema-invalid","name":"StructuredOutput","input":{"color":3}}}),
                );
                event(json!({"type":"content_block_stop","index":1}));
                emit(
                    json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"schema-invalid","is_error":true,"content":"Output does not match required schema"}]}}),
                );
                event(
                    json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}),
                );
                event(json!({"type":"message_stop"}));
                if prompt.contains("SCHEMA_GIVE_UP") {
                    emit(
                        json!({"type":"result","subtype":"error_max_structured_output_retries","is_error":true,"result":""}),
                    );
                    continue;
                }
                sequence += 1;
                message = format!("msg-{sequence}");
                event(
                    json!({"type":"message_start","message":{"id":message,"type":"message","role":"assistant","model":"claude-test","content":[],"usage":{"input_tokens":12,"output_tokens":1}}}),
                );
            }
            event(
                json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"schema-valid","name":"StructuredOutput","input":{}}}),
            );
            event(
                json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"color\":\"blue\"}"}}),
            );
            event(json!({"type":"content_block_stop","index":2}));
            emit(
                json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"schema-valid","is_error":false,"content":"Structured output provided successfully"}]}}),
            );
            event(
                json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}),
            );
            event(json!({"type":"message_stop"}));
            emit(
                json!({"type":"result","subtype":"success","is_error":false,"result":"","structured_output":{"color":"blue"}}),
            );
            continue;
        }
        if sequence == 1 && prompt.contains("WHOLE_TOOL_SPLIT") {
            let wire = tools[0]["name"].as_str().unwrap();
            for content in [
                json!([{"type":"text","text":"calling"}]),
                json!([{"type":"tool_use","id":"split-a","name":format!("mcp__nora__{wire}"),"input":{"q":"first"}}]),
                json!([{"type":"tool_use","id":"split-b","name":format!("mcp__nora__{wire}"),"input":{"q":"second"}}]),
            ] {
                emit(
                    json!({"type":"assistant","message":{"id":id,"role":"assistant","model":"claude-test","content":content,"stop_reason":"tool_use","usage":{"input_tokens":2,"output_tokens":777}}}),
                );
            }
            for (rpc_id, call, q) in [(3, "split-a", "first"), (4, "split-b", "second")] {
                rpc(
                    &mut helper_in,
                    json!({"jsonrpc":"2.0","id":rpc_id,"method":"tools/call","params":{"name":wire,"arguments":{"q":q},"_meta":{"claudecode/toolUseId":call}}}),
                );
            }
            let mut replies = std::collections::BTreeSet::new();
            while replies.len() < 2 {
                if let Ok(value) = from_cli.1.try_recv() {
                    if let Some(rpc_id) = value["id"].as_u64().filter(|id| [3, 4].contains(id)) {
                        replies.insert(rpc_id);
                    }
                }
                if let Ok(queued) = to_cli.1.try_recv() {
                    if queued["type"] == "user" {
                        emit(
                            json!({"type":"command_lifecycle","command_uuid":queued["uuid"],"state":"queued"}),
                        );
                    }
                }
                thread::sleep(Duration::from_millis(2));
            }
            emit(
                json!({"type":"assistant","message":{"id":id,"role":"assistant","model":"claude-test","content":[{"type":"tool_use","id":"split-a","name":format!("mcp__nora__{wire}"),"input":{"q":"first"}}],"stop_reason":"tool_use"}}),
            );
            sequence += 1;
            event(
                json!({"type":"message_start","message":{"id":format!("msg-{sequence}"),"type":"message","role":"assistant","model":"claude-test","content":[],"usage":{"input_tokens":3,"output_tokens":1}}}),
            );
            event(
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"Both caller tools answered."}}),
            );
            event(json!({"type":"content_block_stop","index":0}));
            event(
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":9}}),
            );
            event(json!({"type":"message_stop"}));
            emit(json!({"type":"result","is_error":false,"result":""}));
            continue;
        }
        if prompt.contains("WHOLE_WEB_SEARCH") {
            assert!(args.windows(2).any(|pair| pair == ["--tools", "WebSearch"]));
            let search_id = format!("search-own-{sequence}");
            emit(
                json!({"type":"assistant","message":{"id":id,"type":"message","role":"assistant","model":"claude-test","content":[{"type":"text","text":"Let me look. "},{"type":"tool_use","id":search_id,"name":"WebSearch","input":{"query":"official documentation"}}],"stop_reason":"tool_use","usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":100}}}),
            );
            let mut searched: Value =
                serde_json::from_str(include_str!("testdata/cli-web-search-result.json")).unwrap();
            searched["message"]["content"][0]["tool_use_id"] = json!(search_id);
            emit(searched.clone());
            emit(searched);
            sequence += 1;
            emit(
                json!({"type":"assistant","message":{"id":format!("msg-{sequence}"),"type":"message","role":"assistant","model":"claude-test","content":[{"type":"text","text":"Found [Example](https://example.com)."}],"stop_reason":"end_turn","usage":{"input_tokens":30,"output_tokens":7,"cache_read_input_tokens":50}}}),
            );
            emit(json!({"type":"result","is_error":false,"result":""}));
            continue;
        }
        event(
            json!({"type":"message_start","message":{"id":id,"type":"message","role":"assistant","model":"claude-test","content":[],"usage":{"input_tokens":10,"output_tokens":1,"cache_read_input_tokens":100,"cache_creation_input_tokens":20}}}),
        );
        if prompt.contains("OWN_WEB_SEARCH") {
            let search_id = format!("search-own-{sequence}");
            assert!(args.windows(2).any(|pair| pair == ["--tools", "WebSearch"]));
            event(
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            );
            event(
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Let me look. "}}),
            );
            event(json!({"type":"content_block_stop","index":0}));
            event(
                json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":search_id,"name":"WebSearch","input":{}}}),
            );
            event(
                json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"query\":\"official documentation\"}"}}),
            );
            event(json!({"type":"content_block_stop","index":1}));
            event(
                json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}),
            );
            event(json!({"type":"message_stop"}));
            let mut searched: Value =
                serde_json::from_str(include_str!("testdata/cli-web-search-result.json")).unwrap();
            searched["message"]["content"][0]["tool_use_id"] = json!(search_id);
            emit(searched.clone());
            emit(searched);

            emit(
                json!({"type":"assistant","message":{"id":id,"role":"assistant","content":[{"type":"tool_use","id":search_id,"name":"WebSearch","input":{"query":"official documentation"}}],"stop_reason":"tool_use"}}),
            );
            sequence += 1;
            event(
                json!({"type":"message_start","message":{"id":format!("msg-{sequence}"),"type":"message","role":"assistant","model":"claude-test","usage":{"input_tokens":30,"output_tokens":1,"cache_read_input_tokens":50}}}),
            );
            event(
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            );
            event(
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Found [Example](https://example.com)."}}),
            );
            event(json!({"type":"content_block_stop","index":0}));
            event(
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}),
            );
            event(json!({"type":"message_stop"}));
            emit(json!({"type":"result","is_error":false,"result":""}));
            continue;
        }
        let use_tool = sequence == 1 && prompt.contains("USE_TOOL") && !tools.is_empty();
        if use_tool {
            let wire = tools[0]["name"].as_str().unwrap();
            let mixed = prompt.contains("MIXED_SEARCH");
            let tool_index = usize::from(mixed);
            let mut searched: Value =
                serde_json::from_str(include_str!("testdata/cli-web-search-result.json")).unwrap();
            if mixed {
                assert!(args.windows(2).any(|pair| pair == ["--tools", "WebSearch"]));
                event(
                    json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"search-own","name":"WebSearch","input":{"query":"official documentation"}}}),
                );
                event(json!({"type":"content_block_stop","index":0}));
                emit(searched.clone());
            }
            event(
                json!({"type":"content_block_start","index":tool_index,"content_block":{"type":"tool_use","id":"call-1","name":format!("mcp__nora__{wire}"),"input":{}}}),
            );
            event(
                json!({"type":"content_block_delta","index":tool_index,"delta":{"type":"input_json_delta","partial_json":"{\"q\":\"test\"}"}}),
            );
            event(json!({"type":"content_block_stop","index":tool_index}));
            event(
                json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":2}}),
            );
            event(json!({"type":"message_stop"}));
            emit(
                json!({"type":"assistant","message":{"id":id,"role":"assistant","content":[{"type":"tool_use","id":"call-1","name":format!("mcp__nora__{wire}"),"input":{"q":"test"}}],"stop_reason":"tool_use","usage":{"output_tokens":2}}}),
            );
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
            if mixed {
                emit(searched.take());
            }
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
            if args.windows(2).any(|pair| pair[0] == "--json-schema") {
                emit(
                    json!({"type":"result","subtype":"success","is_error":false,"result":"","structured_output":{"color":"blue"}}),
                );
            } else {
                emit(json!({"type":"result","is_error":false,"result":""}));
            }

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
        if let Ok(bytes)=std::fs::read(directory.join("fixture-late-rate-limit-event.json")) {
            thread::sleep(Duration::from_millis(30));
            if let Ok(value)=serde_json::from_slice::<Value>(&bytes) {emit(value);}
        }
    }
    let _ = helper.kill();
}
