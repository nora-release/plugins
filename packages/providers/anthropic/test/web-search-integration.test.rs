use super::*;

#[tokio::test]
async fn cli_owned_search_stays_inside_one_streamed_or_complete_reply() {
    let directory = fixture::Directory::new();
    let account = directory.0.join("claude-accounts/account-a");
    let credentials = fixture::profile(&account);
    let mut host = Host::new(&directory.0).await;
    for (kind, prompt, streaming) in [
        ("web_search_20250305", "OWN_WEB_SEARCH", false),
        ("web_search_20250305", "OWN_WEB_SEARCH", true),
        ("web_search_20260209", "OWN_WEB_SEARCH", false),
        ("web_search_20260209", "OWN_WEB_SEARCH", true),
        ("web_search_20260318", "OWN_WEB_SEARCH", false),
        ("web_search_20260318", "OWN_WEB_SEARCH", true),
        ("web_search_20250305", "WHOLE_WEB_SEARCH", false),
        ("web_search_20250305", "WHOLE_WEB_SEARCH", true),
    ] {
        let id = host
            .fetch(
                credentials.clone(),
                &format!("{kind}-{prompt}-{streaming}"),
                "turn",
                vec![user(prompt)],
                json!([{"type":kind,"name":"web_search"}]),
                streaming,
            )
            .await;
        let (status, body) = host.response(id).await;
        assert_eq!(status, 200, "{body}");
        assert!(
            body.contains("Let me look.") && body.contains("https://example.com"),
            "{body}"
        );
        assert!(
            !body.contains("WebSearch") && !body.contains("search-own"),
            "{body}"
        );
        let message = if streaming {
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
            let starts = events
                .iter()
                .filter(|event| event["type"] == "content_block_start")
                .collect::<Vec<_>>();
            assert_eq!(
                starts
                    .iter()
                    .map(|event| event["index"].as_u64().unwrap())
                    .collect::<Vec<_>>(),
                vec![0, 1, 2, 3]
            );
            let call = &starts[1]["content_block"];
            let result = &starts[2]["content_block"];
            check_sources(call, result);
            let input = events
                .iter()
                .filter(|event| event["index"] == 1 && event["type"] == "content_block_delta")
                .map(|event| event["delta"]["partial_json"].as_str().unwrap())
                .collect::<String>();
            assert_eq!(
                serde_json::from_str::<Value>(&input).unwrap(),
                json!({"query":"official documentation"})
            );
            let final_delta = events
                .iter()
                .rfind(|event| event["type"] == "message_delta")
                .unwrap();
            assert_eq!(final_delta["delta"]["stop_reason"], "end_turn");
            final_delta.clone()
        } else {
            serde_json::from_str(&body).unwrap()
        };
        if !streaming {
            assert_eq!(message["content"].as_array().unwrap().len(), 4);
            check_sources(&message["content"][1], &message["content"][2]);
            assert_eq!(
                message["content"][1]["input"],
                json!({"query":"official documentation"})
            );
            assert!(!body.contains("_arguments"));
        }
        assert_eq!(message["usage"]["input_tokens"], 40);
        assert_eq!(message["usage"]["output_tokens"], 12);
        assert_eq!(message["usage"]["cache_read_input_tokens"], 150);
    }
    let id = host
        .fetch(
            credentials,
            "ordinary",
            "turn",
            vec![user("no search requested")],
            json!([]),
            false,
        )
        .await;
    assert_eq!(host.response(id).await.0, 200);
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
    assert_eq!(chats.len(), 9);
    assert!(
        chats.iter().all(|chat| chat["apiBase"].is_null()),
        "default search must keep the CLI native endpoint"
    );
    let mut builtins = chats
        .iter()
        .map(|log| {
            log["args"]
                .as_array()
                .unwrap()
                .windows(2)
                .find(|pair| pair[0] == "--tools")
                .unwrap()[1]
                .as_str()
                .unwrap()
        })
        .collect::<Vec<_>>();
    builtins.sort();
    assert_eq!(
        builtins,
        vec![
            "",
            "WebSearch",
            "WebSearch",
            "WebSearch",
            "WebSearch",
            "WebSearch",
            "WebSearch",
            "WebSearch",
            "WebSearch"
        ]
    );
}

#[tokio::test]
async fn subscription_search_options_stay_with_the_original_cli_search() {
    let directory = fixture::Directory::new();
    let account = directory.0.join("claude-accounts/account-a");
    let credentials = fixture::profile(&account);
    let mut host = Host::new(&directory.0).await;
    let id=host.fetch(credentials,"cli-options","one",vec![user("OWN_WEB_SEARCH")],json!([{"type":"web_search_20260318","name":"web_search","max_uses":1,"allowed_domains":["example.com"],"allowed_callers":["code_execution_20260120"],"response_inclusion":"excluded","unknown_option":"ignored"}]),false).await;
    let (status, body) = host.response(id).await;
    assert_eq!(status, 200, "{body}");
    assert!(logs(&account).iter().all(|log| log["apiBase"].is_null()));
    assert!(!body.contains("unknown_option"));
}

#[tokio::test]
async fn version_markers_reuse_the_native_cli_and_cold_history_replays_once() {
    let directory = fixture::Directory::new();
    let account = directory.0.join("claude-accounts/account-a");
    let credentials = fixture::profile(&account);
    let mut host = Host::new(&directory.0).await;
    let mut messages = vec![user("OWN_WEB_SEARCH")];
    let mut search_ids = std::collections::BTreeSet::new();
    for (turn, kind) in [
        ("one", "web_search_20250305"),
        ("two", "web_search_20260209"),
        ("three", "web_search_20260318"),
    ] {
        let id = host
            .fetch(
                credentials.clone(),
                "search-policy-session",
                turn,
                messages.clone(),
                json!([{"type":kind,"name":"web_search","max_uses":null,"allowed_callers":null,"cache_control":null}]),
                false,
            )
            .await;
        let (status, body) = host.response(id).await;
        assert_eq!(status, 200, "{body}");
        let reply: Value = serde_json::from_str(&body).unwrap();
        let calls = reply["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|part| part["type"] == "server_tool_use")
            .collect::<Vec<_>>();
        assert_eq!(
            calls.len(),
            1,
            "replayed results must not reappear in a later reply"
        );
        assert!(search_ids.insert(calls[0]["id"].as_str().unwrap().to_owned()));
        messages.push(json!({"role":"assistant","content":reply["content"]}));
        messages.push(user("OWN_WEB_SEARCH"));
    }
    let id = host
        .fetch(
            credentials,
            "cold-search-session",
            "four",
            messages,
            json!([{"type":"web_search_20260318","name":"web_search"}]),
            false,
        )
        .await;
    let (status, body) = host.response(id).await;
    assert_eq!(status, 200, "{body}");
    let reply: Value = serde_json::from_str(&body).unwrap();
    let calls = reply["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|part| part["type"] == "server_tool_use")
        .collect::<Vec<_>>();
    assert_eq!(
        calls.len(),
        1,
        "cold history replay must not duplicate prior search blocks"
    );
    assert!(search_ids.insert(calls[0]["id"].as_str().unwrap().to_owned()));
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
    assert_eq!(
        chats.len(),
        2,
        "all version markers reuse one CLI; a distinct conversation starts a new one"
    );
    assert!(chats.iter().all(|log| log["apiBase"].is_null()));
    assert!(chats.iter().all(|log| log["args"]
        .as_array()
        .unwrap()
        .windows(2)
        .any(|pair| pair[0] == "--tools" && pair[1] == "WebSearch")));
}

fn check_sources(call: &Value, result: &Value) {
    assert_eq!(call["type"], "server_tool_use");
    assert_eq!(call["name"], "web_search");
    assert!(call["id"].as_str().unwrap().starts_with("srvtoolu_"));
    assert_eq!(result["type"], "web_search_tool_result");
    assert_eq!(result["tool_use_id"], call["id"]);
    assert_eq!(
        result["content"],
        json!([{"type":"web_search_result","title":"Example documentation","url":"https://example.com/docs","page_age":"today","encrypted_content":""}])
    );
}

#[tokio::test]
async fn mixed_search_and_mcp_keep_late_cli_copies_out_of_the_next_fetch() {
    for search in [false, true] {
        let directory = fixture::Directory::new();
        let account = directory.0.join("claude-accounts/account-a");
        let credentials = fixture::profile(&account);
        let mut host = Host::new(&directory.0).await;
        let mut definitions = tools();
        if search {
            definitions
                .as_array_mut()
                .unwrap()
                .push(json!({"type":"web_search_20250305","name":"web_search"}));
        }
        let mut messages = vec![user(if search {
            "USE_TOOL MIXED_SEARCH"
        } else {
            "USE_TOOL"
        })];
        let id = host
            .fetch(
                credentials.clone(),
                "mixed-tool-copy",
                "one",
                messages.clone(),
                definitions.clone(),
                false,
            )
            .await;
        let (status, body) = host.response(id).await;
        assert_eq!(status, 200, "{body}");
        let reply: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(reply["stop_reason"], "tool_use");
        let content = reply["content"].as_array().unwrap();
        let calls = content
            .iter()
            .filter(|part| part["type"] == "tool_use")
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["name"], "lookup.tool");
        assert_eq!(
            content
                .iter()
                .filter(|part| part["type"] == "web_search_tool_result")
                .count(),
            usize::from(search)
        );
        messages.push(assistant(&reply));
        messages.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"call-1","content":"public MCP result"}]}));
        let id = host
            .fetch(
                credentials,
                "mixed-tool-copy",
                "one",
                messages,
                definitions,
                false,
            )
            .await;
        let (status, body) = host.response(id).await;
        assert_eq!(status, 200, "{body}");
        let reply: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(reply["stop_reason"], "end_turn");
        assert!(body.contains("public MCP result"), "{body}");
        assert!(
            reply["content"]
                .as_array()
                .unwrap()
                .iter()
                .all(|part| part["type"] == "text"),
            "late copies and search results must not be emitted again: {body}"
        );
    }
}

#[tokio::test]
async fn whole_same_id_fragments_park_all_caller_tools_and_resume_once() {
    let directory = fixture::Directory::new();
    let account = directory.0.join("claude-accounts/account-a");
    let credentials = fixture::profile(&account);
    let mut host = Host::new(&directory.0).await;
    let mut messages = vec![user("WHOLE_TOOL_SPLIT")];
    let id = host
        .fetch(
            credentials.clone(),
            "whole-split",
            "one",
            messages.clone(),
            tools(),
            false,
        )
        .await;
    let (status, body) = host.response(id).await;
    assert_eq!(status, 200, "{body}");
    let reply: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(reply["stop_reason"], "tool_use");
    let blocks = reply["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 3);
    assert_eq!(blocks[0]["text"], "calling");
    assert_eq!(blocks[1]["id"], "split-a");
    assert_eq!(blocks[2]["id"], "split-b");
    assert_eq!(blocks[2]["input"], json!({"q":"second"}));
    assert_eq!(reply["usage"]["output_tokens"], 777);
    messages.push(assistant(&reply));
    messages.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"split-a","content":"first result"},{"type":"tool_result","tool_use_id":"split-b","content":"second result"}]}));
    let id = host
        .fetch(credentials, "whole-split", "one", messages, tools(), false)
        .await;
    let (status, body) = host.response(id).await;
    assert_eq!(status, 200, "{body}");
    let reply: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(reply["stop_reason"], "end_turn");
    assert_eq!(
        reply["content"],
        json!([{"type":"text","text":"Both caller tools answered."}])
    );
}
