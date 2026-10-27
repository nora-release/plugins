use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    process::Stdio,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};
#[path = "fixture.rs"]
mod fixture;
#[path = "learned-usage-integration.test.rs"]
mod learned_usage_integration;
#[path = "login-integration.test.rs"]
mod login_integration;
#[path = "refusal-integration.test.rs"]
mod refusal_integration;
#[path = "session-activity-integration.test.rs"]
mod session_activity_integration;
#[path = "session-integration.test.rs"]
mod session_integration;
#[path = "structured-output-integration.test.rs"]
mod structured_output_integration;
#[path = "web-search-integration.test.rs"]
mod web_search_integration;
struct Host {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    queued: BTreeMap<u64, VecDeque<Value>>,
    next: u64,
    updates: Vec<Value>,
    accept: bool,
}
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}
impl Host {
    async fn new(root: &std::path::Path) -> Self {
        Self::with_environment(root, &[]).await
    }
    async fn with_environment(
        root: &std::path::Path,
        environment: &[(&str, std::ffi::OsString)],
    ) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_nora-provider-anthropic"));
        command.envs(environment.iter().map(|(key, value)| (*key, value)));
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
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
            queued: BTreeMap::new(),
            next: 1,
            updates: Vec::new(),
            accept: true,
        };
        let manifest: Value = serde_json::from_str(include_str!("../nora-provider.json")).unwrap();
        let id=host.send("initialize",json!({"apiVersion":1,"pluginID":"anthropic","pluginDirectory":env!("CARGO_MANIFEST_DIR"),"dataDirectory":root,"manifest":manifest,"hostCapabilities":{"credentialUpdates":true}})).await;
        assert!(host.frame(id).await["result"]["capabilities"]["persistent"] == true);
        host
    }
    async fn send(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next;
        self.next += 1;
        self.raw(json!({"id":id,"method":method,"params":params}))
            .await;
        id
    }
    async fn raw(&mut self, value: Value) {
        self.input
            .write_all(format!("{value}\n").as_bytes())
            .await
            .unwrap();
        self.input.flush().await.unwrap();
    }
    async fn frame(&mut self, id: u64) -> Value {
        if let Some(frame) = self.queued.get_mut(&id).and_then(VecDeque::pop_front) {
            return frame;
        }
        loop {
            let mut line = String::new();
            let read = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                self.output.read_line(&mut line),
            )
            .await
            .expect("provider response timeout")
            .unwrap();
            assert!(read > 0, "provider ended");
            let value: Value = serde_json::from_str(&line).unwrap();
            if value["event"] == "credentials" {
                self.updates.push(value["credentials"].clone());
                self.raw(json!({"method":"credentialsSaved","params":{"id":value["id"],"updateID":value["updateID"],"success":self.accept}})).await;
                continue;
            }
            let received = value["id"].as_u64().unwrap();
            if received == id {
                return value;
            }
            self.queued.entry(received).or_default().push_back(value);
        }
    }
    async fn fetch(
        &mut self,
        credentials: Value,
        session: &str,
        turn: &str,
        messages: Vec<Value>,
        tools: Value,
        stream: bool,
    ) -> u64 {
        let body = json!({"model":"claude-test","max_tokens":1024,"stream":stream,"system":"Use the supplied tools only.","messages":messages,"tools":tools});
        self.send("fetch",json!({"account":"work","credentials":credentials,"networkProxy":{"mode":"direct"},"url":"https://api.anthropic.com/v1/messages","method":"POST","headers":{"authorization":"caller-secret"},"metadata":{"sessionID":session,"turnID":turn},"body":STANDARD.encode(body.to_string())})).await
    }
    async fn response(&mut self, id: u64) -> (u16, String) {
        let mut status = 0;
        let mut bytes = Vec::new();
        loop {
            let frame = self.frame(id).await;
            if let Some(error) = frame.get("error") {
                panic!("provider error: {error}");
            }
            match frame["event"].as_str() {
                Some("head") => status = frame["status"].as_u64().unwrap() as u16,
                Some("chunk") => {
                    bytes.extend(STANDARD.decode(frame["data"].as_str().unwrap()).unwrap())
                }
                Some("done") => return (status, String::from_utf8(bytes).unwrap()),
                _ => panic!("unexpected frame: {frame}"),
            }
        }
    }
}
fn tools() -> Value {
    json!([{"name":"lookup.tool","description":"Look up one item","input_schema":{"type":"object","properties":{"q":{"type":"string"}},"required":["q"]}}])
}
fn user(text: &str) -> Value {
    json!({"role":"user","content":text})
}
fn assistant(response: &Value) -> Value {
    json!({"role":"assistant","content":response["content"]})
}
fn logs(directory: &std::path::Path) -> Vec<Value> {
    let mut files = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("fixture-process-") && name.ends_with(".json"))
        })
        .collect::<Vec<_>>();
    files.sort();
    files
        .into_iter()
        .map(|path| serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap())
        .collect()
}
#[tokio::test]
async fn compiled_cli_and_real_stdio_mcp_keep_paired_tools_images_user_words_and_three_turns() {
    let directory = fixture::Directory::new();
    let account = directory.0.join("claude-accounts/account-a");
    let credentials = fixture::profile(&account);
    let mut host = Host::new(&directory.0).await;
    let mut messages = vec![user("USE_TOOL")];
    let id = host
        .fetch(
            credentials.clone(),
            "conversation-a",
            "turn-1",
            messages.clone(),
            tools(),
            false,
        )
        .await;
    let (status, body) = host.response(id).await;
    assert_eq!(status, 200);
    let first: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(first["stop_reason"], "tool_use");
    assert_eq!(first["content"][0]["id"], "call-1");
    assert_eq!(first["content"][0]["name"], "lookup.tool");
    assert_eq!(first["content"][0]["input"], json!({"q":"test"}));
    messages.push(assistant(&first));
    messages.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"call-1","content":[{"type":"text","text":"tool-result-word"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"aW1hZ2U="}}]},{"type":"text","text":"Use this new user instruction."}]}));
    let id = host
        .fetch(
            credentials.clone(),
            "conversation-a",
            "turn-1",
            messages.clone(),
            tools(),
            false,
        )
        .await;
    let (status, body) = host.response(id).await;
    assert_eq!(status, 200);
    let second: Value = serde_json::from_str(&body).unwrap();
    assert!(second["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("tool-result-word"));
    assert!(body.contains("image/png"));
    assert!(body.contains("Use this new user instruction"));
    assert_eq!(second["usage"]["cache_read_input_tokens"], 111);
    messages.push(assistant(&second));
    messages.push(user("next user turn"));
    let id = host
        .fetch(
            credentials,
            "conversation-a",
            "turn-2",
            messages,
            tools(),
            false,
        )
        .await;
    let (status, body) = host.response(id).await;
    assert_eq!(status, 200);
    assert!(body.contains("answer 3"));
    let logs = logs(&account);
    let chats = logs
        .iter()
        .filter(|record| {
            record["args"]
                .as_array()
                .unwrap()
                .iter()
                .any(|arg| arg == "stream-json")
        })
        .collect::<Vec<_>>();
    assert_eq!(chats.len(), 1);
    let args = chats[0]["args"].as_array().unwrap();
    assert!(args
        .windows(2)
        .any(|pair| pair[0] == "--tools" && pair[1] == ""));
    assert!(args
        .windows(2)
        .any(|pair| pair[0] == "--setting-sources" && pair[1] == ""));
    assert!(args.iter().any(|arg| arg == "--strict-mcp-config"));
    assert_eq!(chats[0]["builtinMCP"], "0");
    assert_eq!(chats[0]["memory"], "1");
    assert_eq!(chats[0]["proxies"]["NO_PROXY"], "*");
    assert_eq!(chats[0]["proxies"]["HTTPS_PROXY"], "");
    assert!(!body.contains("fixture-refresh"));
}
#[tokio::test]
async fn stale_or_foreign_tool_result_ids_cannot_unblock_a_parked_cli() {
    let directory = fixture::Directory::new();
    let credentials = fixture::profile(&directory.0.join("claude-accounts/account-a"));
    let mut host = Host::new(&directory.0).await;
    let mut messages = vec![user("USE_TOOL")];
    let id = host
        .fetch(
            credentials.clone(),
            "a",
            "t",
            messages.clone(),
            tools(),
            false,
        )
        .await;
    let (_, body) = host.response(id).await;
    messages.push(assistant(&serde_json::from_str(&body).unwrap()));
    messages.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"foreign-call","content":"wrong"}]}));
    let id = host
        .fetch(credentials, "a", "t", messages, tools(), false)
        .await;
    let frame = host.frame(id).await;
    assert_eq!(frame["error"]["code"], "invalid_request");
}
#[tokio::test]
async fn early_limits_are_http_429_and_errors_after_output_are_terminal_without_replay() {
    let directory = fixture::Directory::new();
    let account = directory.0.join("claude-accounts/account-a");
    let credentials = fixture::profile(&account);
    let mut host = Host::new(&directory.0).await;
    let id = host
        .fetch(
            credentials.clone(),
            "limit",
            "t",
            vec![user("EARLY_LIMIT")],
            json!([]),
            true,
        )
        .await;
    let (status, body) = host.response(id).await;
    assert_eq!(status, 429);
    assert!(body.contains("rate_limit_error"));
    assert!(!body.contains("private-access"));
    let id = host
        .fetch(
            credentials,
            "late",
            "t",
            vec![user("LATE_ERROR")],
            json!([]),
            true,
        )
        .await;
    let (status, body) = host.response(id).await;
    assert_eq!(status, 200);
    assert!(body.contains("event: error"));
    assert!(!body.contains("message_stop"));
    assert!(!body.contains("private-refresh"));
    let chats = logs(&account)
        .into_iter()
        .filter(|record| {
            record["args"]
                .as_array()
                .unwrap()
                .iter()
                .any(|arg| arg == "stream-json")
        })
        .count();
    assert_eq!(chats, 2);
}
#[tokio::test]
async fn credential_rotation_is_saved_over_private_ack_events_and_never_returned_in_http() {
    let directory = fixture::Directory::new();
    let credentials = fixture::profile(&directory.0.join("claude-accounts/account-a"));
    let mut host = Host::new(&directory.0).await;
    let id = host
        .fetch(
            credentials,
            "rotation",
            "t",
            vec![user("ROTATE")],
            json!([]),
            true,
        )
        .await;
    let (status, body) = host.response(id).await;
    assert_eq!(status, 200);
    assert_eq!(host.updates.len(), 1);
    assert_eq!(host.updates[0]["access"], "next-access");
    assert_eq!(host.updates[0]["refresh"], "next-refresh");
    assert!(!body.contains("next-refresh"));
    assert!(!body.contains("next-access"));
}
#[tokio::test]
async fn rejected_rotation_ack_ends_the_reply_and_does_not_report_success() {
    let directory = fixture::Directory::new();
    let credentials = fixture::profile(&directory.0.join("claude-accounts/account-a"));
    let mut host = Host::new(&directory.0).await;
    host.accept = false;
    let id = host
        .fetch(
            credentials,
            "rotation",
            "t",
            vec![user("ROTATE")],
            json!([]),
            true,
        )
        .await;
    let (_, body) = host.response(id).await;
    assert!(body.contains("event: error"));
    assert!(!body.contains("message_stop"));
    assert!(!body.contains("next-refresh"));
}
#[tokio::test]
async fn real_cli_authentication_uses_an_isolated_profile_and_the_browser_opener() {
    let directory = fixture::Directory::new();
    let mut host = Host::new(&directory.0).await;
    let id=host.send("authorize",json!({"authMethodID":"oauth","inputs":{"claudePath":fixture::binary()},"networkProxy":{"mode":"direct"}})).await;
    let frame = host.frame(id).await;
    assert!(frame.get("error").is_none(), "{frame}");
    let login = frame["result"].clone();
    assert!(login["url"]
        .as_str()
        .unwrap()
        .starts_with("https://claude.ai/oauth/authorize?"));
    let id = host
        .send("callback", json!({"session":login["session"]}))
        .await;
    let frame = host.frame(id).await;
    assert!(frame.get("error").is_none(), "{frame}");
    let credentials = &frame["result"]["credentials"];
    assert_eq!(credentials["email"], "person@example.test");
    assert_eq!(
        credentials["extras"]["noraClaudeCLI"]["organization"],
        "Example"
    );
    assert!(std::path::Path::new(
        credentials["extras"]["noraClaudeCLI"]["directory"]
            .as_str()
            .unwrap()
    )
    .starts_with(&directory.0));
}
#[tokio::test]
async fn usage_comes_from_the_real_cli_command_and_retains_model_scopes() {
    let directory = fixture::Directory::new();
    let credentials = fixture::profile(&directory.0.join("claude-accounts/account-a"));
    let mut host = Host::new(&directory.0).await;
    let id = host
        .send(
            "usage",
            json!({"account":"work","credentials":credentials,"networkProxy":{"mode":"direct"}}),
        )
        .await;
    let frame = host.frame(id).await;
    assert!(frame.get("error").is_none(), "{frame}");
    let items = frame["result"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["usedPercent"], 25.0);
    assert_eq!(items[2]["modelIDs"], json!(["*opus*"]));
}
#[tokio::test]
async fn cli_output_usage_accepts_verbose_results_and_nonzero_success_results() {
    for (exit, payload) in [
        (
            0,
            json!([{"type":"system","result":"ignore this"},{"type":"result","is_error":true,"result":"old error"},{"type":"result","is_error":false,"result":"Current session: 17% used"}]),
        ),
        (
            1,
            json!({"type":"result","is_error":false,"result":"Current session: 17% used"}),
        ),
    ] {
        let directory = fixture::Directory::new();
        let path = directory.0.join("claude-accounts/account-a");
        let credentials = fixture::profile(&path);
        std::fs::write(
            path.join("fixture-usage.json"),
            json!({"exit":exit,"stdout":payload.to_string()}).to_string(),
        )
        .unwrap();
        let mut host = Host::new(&directory.0).await;
        let id = host.send("usage", json!({"account":"work","credentials":credentials,"networkProxy":{"mode":"direct"}})).await;
        let frame = host.frame(id).await;
        assert!(frame.get("error").is_none(), "{frame}");
        assert_eq!(frame["result"]["items"][0]["usedPercent"], 17.0);
    }
}
#[tokio::test]
async fn cli_output_usage_preserves_error_categories_and_redacts_private_credentials() {
    for (result, stderr, expected_code, expected_text) in [
        (
            None,
            "Network request failed: fixture-access fixture-refresh sk-ant-oat01-new-private-value",
            "upstream_transport_error",
            "Network request failed",
        ),
        (
            Some("Failed to refresh OAuth token: another Claude Code process is refreshing it"),
            "",
            "upstream_transport_error",
            "another Claude Code process",
        ),
        (
            Some("OAuth session expired and could not be refreshed: fixture-access"),
            "",
            "authentication_required",
            "Sign in",
        ),
    ] {
        let directory = fixture::Directory::new();
        let path = directory.0.join("claude-accounts/account-a");
        let credentials = fixture::profile(&path);
        let stdout = result
            .map(|text| json!({"type":"result","is_error":true,"result":text}).to_string())
            .unwrap_or_default();
        std::fs::write(
            path.join("fixture-usage.json"),
            json!({"exit":1,"stdout":stdout,"stderr":stderr}).to_string(),
        )
        .unwrap();
        let mut host = Host::new(&directory.0).await;
        let id = host.send("usage", json!({"account":"work","credentials":credentials,"networkProxy":{"mode":"direct"}})).await;
        let frame = host.frame(id).await;
        assert_eq!(frame["error"]["code"], expected_code, "{frame}");
        let message = frame["error"]["message"].as_str().unwrap();
        assert!(message.contains(expected_text), "{frame}");
        for secret in [
            "fixture-access",
            "fixture-refresh",
            "sk-ant-oat01-new-private-value",
        ] {
            assert!(!message.contains(secret), "{frame}");
        }
    }
}
#[tokio::test]
async fn cli_output_usage_reports_structured_errors_and_cli_failures_without_expired_login() {
    for (exit, stdout, stderr, expected) in [
        (
            0,
            json!({"type":"result","is_error":true,"result":"Network request failed"}).to_string(),
            "",
            "Network request failed",
        ),
        (
            1,
            json!([{"type":"result","is_error":true,"errors":["Connection timed out"]}])
                .to_string(),
            "",
            "Connection timed out",
        ),
        (
            2,
            String::new(),
            "unknown option '--strict-mcp-config'",
            "unknown option",
        ),
        (1, String::new(), "", "stopped without a result"),
    ] {
        let directory = fixture::Directory::new();
        let path = directory.0.join("claude-accounts/account-a");
        let credentials = fixture::profile(&path);
        std::fs::write(
            path.join("fixture-usage.json"),
            json!({"exit":exit,"stdout":stdout,"stderr":stderr}).to_string(),
        )
        .unwrap();
        let mut host = Host::new(&directory.0).await;
        let id = host.send("usage", json!({"account":"work","credentials":credentials,"networkProxy":{"mode":"direct"}})).await;
        let frame = host.frame(id).await;
        assert_eq!(
            frame["error"]["code"], "upstream_transport_error",
            "{frame}"
        );
        assert!(
            frame["error"]["message"]
                .as_str()
                .unwrap()
                .contains(expected),
            "{frame}"
        );
    }
}
#[tokio::test]
async fn cli_output_usage_trusts_explicit_authentication_failures_without_guessing_from_forbidden()
{
    for (payload, expected_code) in [
        (
            json!({"type":"result","is_error":true,"api_error_status":401,"result":"Unauthorized"}),
            "authentication_required",
        ),
        (
            json!({"type":"result","is_error":true,"error":{"type":"authentication_error","message":"Unauthorized"}}),
            "authentication_required",
        ),
        (
            json!({"type":"result","is_error":true,"api_error_status":403,"result":"Forbidden"}),
            "upstream_transport_error",
        ),
        (
            json!({"type":"result","is_error":true,"result":"Unauthorized"}),
            "upstream_transport_error",
        ),
    ] {
        let directory = fixture::Directory::new();
        let path = directory.0.join("claude-accounts/account-a");
        let credentials = fixture::profile(&path);
        std::fs::write(
            path.join("fixture-usage.json"),
            json!({"exit":1,"stdout":payload.to_string()}).to_string(),
        )
        .unwrap();
        let mut host = Host::new(&directory.0).await;
        let id = host.send("usage", json!({"account":"work","credentials":credentials,"networkProxy":{"mode":"direct"}})).await;
        let frame = host.frame(id).await;
        assert_eq!(frame["error"]["code"], expected_code, "{frame}");
    }
}
#[tokio::test]
async fn cancelling_one_conversation_does_not_cancel_another_account_or_its_response() {
    let directory = fixture::Directory::new();
    let first = directory.0.join("claude-accounts/account-a");
    let second = directory.0.join("claude-accounts/account-b");
    let credentials = fixture::profile(&first);
    let other = fixture::profile_with_identity(&second, "account-b");
    assert_ne!(credentials["accountId"], other["accountId"]);
    let mut host = Host::new(&directory.0).await;
    let waiting = host
        .fetch(
            credentials,
            "waiting",
            "t",
            vec![user("WAIT_CANCEL")],
            json!([]),
            true,
        )
        .await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let waiting_pid = loop {
        if let Some(path) = std::fs::read_dir(&first)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with("fixture-waiting-") && name.ends_with(".json")
                    })
            })
        {
            let record: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            break record["pid"].as_u64().unwrap() as u32;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    let id = host
        .fetch(other, "other", "t", vec![user("hello")], json!([]), false)
        .await;
    let abort = host.send("abort", json!({"id":waiting})).await;
    assert_eq!(host.frame(waiting).await["error"]["code"], "cancelled");
    assert_eq!(host.frame(abort).await["result"]["aborted"], true);
    let (status, body) = host.response(id).await;
    assert_eq!(status, 200);
    assert!(body.contains("answer 1"));
    #[cfg(unix)]
    {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if unsafe { libc::kill(waiting_pid as i32, 0) } < 0 {
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::ESRCH)
                );
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the cancelled CLI is still alive"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let records = logs(&second);
        let surviving = records
            .iter()
            .find(|record| {
                record["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|arg| arg == "stream-json")
            })
            .unwrap()["pid"]
            .as_u64()
            .unwrap();
        assert_eq!(
            unsafe { libc::kill(surviving as i32, 0) },
            0,
            "another account's CLI was terminated"
        );
    }
}

#[tokio::test]
async fn api_keys_keep_the_original_http_transport_without_starting_claude() {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let captured = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut byte = [0];
            socket.read_exact(&mut byte).await.unwrap();
            bytes.push(byte[0]);
            if bytes.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let headers = String::from_utf8(bytes).unwrap();
        let length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .unwrap();
        let mut body = vec![0; length];
        socket.read_exact(&mut body).await.unwrap();
        let reply=json!({"id":"message-api","type":"message","role":"assistant","model":"claude-test","content":[{"type":"text","text":"api answer"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":2}}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",reply.len()).as_bytes()).await.unwrap();
        (headers, body)
    });
    let directory = fixture::Directory::new();
    let mut host = Host::new(&directory.0).await;
    let mut manifest: Value = serde_json::from_str(include_str!("../nora-provider.json")).unwrap();
    manifest["baseURL"] = json!(base);
    let id=host.send("initialize",json!({"apiVersion":1,"pluginID":"anthropic","pluginDirectory":env!("CARGO_MANIFEST_DIR"),"dataDirectory":directory.0,"manifest":manifest,"hostCapabilities":{"credentialUpdates":true}})).await;
    assert!(host.frame(id).await.get("error").is_none());
    let original_body = json!({"model":"claude-test","max_tokens":10,"messages":[{"role":"user","content":"hello"}],"tools":[{"type":"web_search_20260318","name":"web_search","max_uses":1,"allowed_domains":["example.com"],"allowed_callers":["code_execution_20260120"],"response_inclusion":"excluded","cache_control":{"type":"ephemeral"},"strict":false}]});
    let id=host.send("fetch",json!({"credentials":{"type":"apiKey","provider":"apiKey","credentialProvider":"apiKey","access":"sk-private-api-key"},"networkProxy":{"mode":"direct"},"url":format!("{base}/v1/messages"),"method":"POST","headers":{"content-type":"application/json"},"body":STANDARD.encode(original_body.to_string())})).await;
    let (status, body) = host.response(id).await;
    assert_eq!(status, 200);
    assert!(body.contains("api answer"));
    let (headers, body) = captured.await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        original_body,
        "API-key search versions and options must be sent unchanged"
    );
    assert!(headers.contains("sk-private-api-key"));
    assert!(!String::from_utf8(body)
        .unwrap()
        .contains("You are Claude Code"));
    assert!(std::fs::read_dir(directory.0.join("claude-accounts"))
        .unwrap()
        .next()
        .is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn killing_plugin_parent_or_disabling_its_group_reaps_only_its_claude_and_mcp() {
    for kill_group in [false, true] {
        let directory = fixture::Directory::new();
        let victim_root = directory.0.join("victim");
        let survivor_root = directory.0.join("survivor");
        let victim_profile = victim_root.join("claude-accounts/a");
        let survivor_profile = survivor_root.join("claude-accounts/b");
        let credentials = fixture::profile(&victim_profile);
        let other = fixture::profile_with_identity(&survivor_profile, "account-b");
        let mut victim = Host::new(&victim_root).await;
        let mut survivor = Host::new(&survivor_root).await;
        let _waiting = victim
            .fetch(
                credentials,
                "waiting",
                "t",
                vec![user("WAIT_CANCEL")],
                tools(),
                false,
            )
            .await;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        let ready: Value = loop {
            let marker = std::fs::read_dir(&victim_profile)
                .unwrap()
                .flatten()
                .map(|entry| entry.path())
                .find(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name.starts_with("fixture-waiting-") && name.ends_with(".json")
                        })
                });
            if let Some(marker) = marker {
                break serde_json::from_slice(&std::fs::read(marker).unwrap()).unwrap();
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "Claude did not reach the pending request"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        let cli_pid = ready["pid"].as_u64().unwrap() as i32;
        let mcp_pid = ready["helper"].as_u64().unwrap() as i32;
        let group = unsafe { libc::getpgid(cli_pid) };
        assert!(group > 0);
        assert_eq!(unsafe { libc::getpgid(mcp_pid) }, group);
        assert_ne!(group, unsafe { libc::getpgrp() });
        let id = survivor
            .fetch(
                other.clone(),
                "other",
                "turn-1",
                vec![user("hello")],
                json!([]),
                false,
            )
            .await;
        let (status, body) = survivor.response(id).await;
        assert_eq!(status, 200);
        let first: Value = serde_json::from_str(&body).unwrap();
        let parent = victim.child.id().unwrap() as i32;
        assert_eq!(
            unsafe { libc::kill(if kill_group { -parent } else { parent }, libc::SIGKILL) },
            0
        );
        victim.child.wait().await.unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if [cli_pid, mcp_pid]
                .into_iter()
                .all(|pid| unsafe { libc::kill(pid, 0) } < 0)
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the killed plugin left a Claude or MCP process alive"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let id = survivor
            .fetch(
                other,
                "other",
                "turn-2",
                vec![user("hello"), assistant(&first), user("another turn")],
                json!([]),
                false,
            )
            .await;
        let (status, body) = survivor.response(id).await;
        assert_eq!(status, 200);
        assert!(
            body.contains("answer 2"),
            "the independent account lost its live CLI conversation"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn private_supervisor_rejects_an_unowned_process_group() {
    let status = Command::new(env!("CARGO_BIN_EXE_nora-provider-anthropic"))
        .args(["claude-supervisor", "3", "/usr/bin/true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .unwrap();
    assert!(!status.success());
}
