use super::*;

fn schema() -> Value {
    json!({"type":"object","properties":{"color":{"type":"string"}},"required":["color"],"additionalProperties":false})
}
async fn configured_fetch(
    host: &mut Host,
    credentials: Value,
    session: &str,
    messages: Vec<Value>,
    tools: Value,
    extra: Value,
    streaming: bool,
) -> u64 {
    let mut body = json!({"model":"claude-test","max_tokens":1024,"stream":streaming,"system":"Use the supplied tools only.","messages":messages,"tools":tools});
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    host.send("fetch",json!({"account":"work","credentials":credentials,"networkProxy":{"mode":"direct"},"url":"https://api.anthropic.com/v1/messages","method":"POST","metadata":{"sessionID":session,"turnID":"one"},"body":STANDARD.encode(body.to_string())})).await
}
fn format() -> Value {
    json!({"output_config":{"format":{"type":"json_schema","schema":schema()}}})
}
fn response_text(body: &str, streaming: bool) -> String {
    if !streaming {
        let message: Value = serde_json::from_str(body).unwrap();
        assert_eq!(message["stop_reason"], "end_turn");
        return message["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect();
    }
    let events = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|data| serde_json::from_str::<Value>(data).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "message_start")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "message_stop")
            .count(),
        1
    );
    events
        .iter()
        .filter_map(|event| event.pointer("/delta/text").and_then(Value::as_str))
        .collect()
}

#[tokio::test]
async fn cli_native_schema_returns_only_validated_json_after_internal_retries() {
    let directory = fixture::Directory::new();
    let account = directory.0.join("claude-accounts/account-a");
    let credentials = fixture::profile(&account);
    let mut host = Host::new(&directory.0).await;
    for prompt in ["SCHEMA_VALID", "SCHEMA_RETRY", "SCHEMA_WORDS_FIRST"] {
        for streaming in [false, true] {
            let id = configured_fetch(
                &mut host,
                credentials.clone(),
                &format!("{prompt}-{streaming}"),
                vec![user(prompt)],
                json!([]),
                format(),
                streaming,
            )
            .await;
            let (status, body) = host.response(id).await;
            assert_eq!(status, 200, "{body}");
            assert_eq!(
                serde_json::from_str::<Value>(&response_text(&body, streaming)).unwrap(),
                json!({"color":"blue"})
            );
            for internal in [
                "not valid JSON",
                "StructuredOutput",
                "schema-invalid",
                "color\\\":3",
            ] {
                assert!(!body.contains(internal), "{body}");
            }
        }
    }
    let chats = logs(&account)
        .into_iter()
        .filter(|log| {
            log["args"]
                .as_array()
                .unwrap()
                .iter()
                .any(|arg| arg == "--mcp-config")
        })
        .collect::<Vec<_>>();
    assert_eq!(chats.len(), 6);
    for chat in chats {
        let args = chat["args"].as_array().unwrap();
        let value = args
            .windows(2)
            .find(|pair| pair[0] == "--json-schema")
            .unwrap()[1]
            .as_str()
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(value).unwrap(), schema());
    }
}

#[tokio::test]
async fn absent_or_rejected_schema_results_report_a_clear_failure() {
    let directory = fixture::Directory::new();
    let credentials = fixture::profile(&directory.0.join("claude-accounts/account-a"));
    let mut host = Host::new(&directory.0).await;
    for prompt in ["SCHEMA_MISSING", "SCHEMA_GIVE_UP"] {
        for streaming in [false, true] {
            let id = configured_fetch(
                &mut host,
                credentials.clone(),
                &format!("{prompt}-{streaming}"),
                vec![user(prompt)],
                json!([]),
                format(),
                streaming,
            )
            .await;
            let (status, body) = host.response(id).await;
            assert_eq!(status, 502, "{body}");
            assert!(body.contains("did not return an answer matching the requested JSON Schema"));
            assert!(!body.contains("not valid JSON") && !body.contains("schema-invalid"));
        }
    }
}

#[tokio::test]
async fn native_schema_keeps_caller_mcp_rounds_separate_from_structured_output() {
    let directory = fixture::Directory::new();
    let credentials = fixture::profile(&directory.0.join("claude-accounts/account-a"));
    let mut host = Host::new(&directory.0).await;
    let mut messages = vec![user("USE_TOOL SCHEMA_CLIENT_TOOL")];
    let id = configured_fetch(
        &mut host,
        credentials.clone(),
        "schema-mcp",
        messages.clone(),
        tools(),
        format(),
        false,
    )
    .await;
    let (status, body) = host.response(id).await;
    assert_eq!(status, 200, "{body}");
    let reply: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(reply["stop_reason"], "tool_use");
    assert_eq!(reply["content"][0]["name"], "lookup.tool");
    messages.push(assistant(&reply));
    messages.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"call-1","content":"caller result"}]}));
    let id = configured_fetch(
        &mut host,
        credentials,
        "schema-mcp",
        messages,
        tools(),
        format(),
        false,
    )
    .await;
    let (status, body) = host.response(id).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&response_text(&body, false)).unwrap(),
        json!({"color":"blue"})
    );
}

#[tokio::test]
async fn tool_choice_none_named_and_required_control_the_cli_mcp_offers() {
    let directory = fixture::Directory::new();
    let credentials = fixture::profile(&directory.0.join("claude-accounts/account-a"));
    let mut host = Host::new(&directory.0).await;
    for (name, prompt, choice) in [
        ("none", "TOOL_CHOICE_NONE", json!({"type":"none"})),
        (
            "named",
            "USE_TOOL TOOL_CHOICE_NAMED",
            json!({"type":"tool","name":"selected"}),
        ),
        (
            "required",
            "USE_TOOL TOOL_CHOICE_REQUIRED",
            json!({"type":"any"}),
        ),
    ] {
        let definitions = json!([{"name":"selected","input_schema":{"type":"object"}},{"name":"other","input_schema":{"type":"object"}}]);
        let mut messages = vec![user(prompt)];
        let id = configured_fetch(
            &mut host,
            credentials.clone(),
            name,
            messages.clone(),
            definitions.clone(),
            json!({"tool_choice":choice}),
            false,
        )
        .await;
        let (status, body) = host.response(id).await;
        assert_eq!(status, 200, "{body}");
        let reply: Value = serde_json::from_str(&body).unwrap();
        if name == "none" {
            assert_eq!(reply["stop_reason"], "end_turn");
            assert!(reply["content"]
                .as_array()
                .unwrap()
                .iter()
                .all(|part| part["type"] != "tool_use"));
            continue;
        }
        assert_eq!(reply["stop_reason"], "tool_use");
        assert_eq!(reply["content"][0]["name"], "selected");
        messages.push(assistant(&reply));
        messages.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"call-1","content":"caller result"}]}));
        let id = configured_fetch(
            &mut host,
            credentials.clone(),
            name,
            messages,
            definitions,
            json!({"tool_choice":choice}),
            false,
        )
        .await;
        assert_eq!(host.response(id).await.0, 200);
    }
}
