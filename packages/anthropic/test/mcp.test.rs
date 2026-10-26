use super::*;

#[test]
fn native_web_search_is_not_exposed_as_a_caller_mcp_tool() {
    let (tools, names) = tools(&json!({"tools":[{"type":"web_search_20250305","name":"web_search"},{"name":"read","input_schema":{"type":"object"}}]})).unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(names.len(), 1);
    assert_eq!(tools[0]["name"], "read");
}
use crate::fixture;
#[tokio::test]
async fn authenticated_callback_waits_for_exact_declared_call_and_keeps_results_separate() {
    let directory = fixture::Directory::new();
    let request = json!({"tools":[{"name":"mcp.tool","input_schema":{"type":"object"}}]});
    let (tools, names) = tools(&request).unwrap();
    let bridge = Bridge::new(&directory.0, tools, names).await.unwrap();
    let config = cli::read(&bridge.path).unwrap();
    let wire = tool_name("mcp.tool");
    let callback_config = config.clone();
    let request = json!({"method":"tools/call","id":"call-a","name":wire,"arguments":{"x":1}});
    let task = tokio::spawn(async move { callback(&callback_config, request).await });
    bridge
        .declare("call-a", &format!("{PREFIX}{wire}"), json!({"x":1}))
        .await
        .unwrap();
    assert!(bridge.deliver("foreign", json!({})).await.is_err());
    bridge
        .deliver(
            "call-a",
            json!({"content":[{"type":"text","text":"done"}],"isError":false}),
        )
        .await
        .unwrap();
    assert_eq!(
        task.await.unwrap().unwrap()["result"]["content"][0]["text"],
        "done"
    );
}
#[tokio::test]
async fn unauthenticated_or_undeclared_tools_never_enter_the_pending_queue() {
    let directory = fixture::Directory::new();
    let (tools, names) = tools(&json!({"tools":[{"name":"allowed"}]})).unwrap();
    let bridge = Bridge::new(&directory.0, tools, names).await.unwrap();
    let mut config = cli::read(&bridge.path).unwrap();
    config["token"] = json!("wrong-token");
    assert!(callback(&config, json!({"method":"tools/list"}))
        .await
        .is_err());
    assert!(bridge
        .declare("call", &format!("{PREFIX}unadvertised"), json!({}))
        .await
        .is_err());
    assert!(bridge.state.lock().await.pending.is_empty());
}
#[test]
fn names_are_collision_resistant_within_claudes_complete_mcp_name_limit() {
    for original in [
        "tool",
        "a.b/c",
        &"verylong".repeat(30),
        "a_123456abcdef",
        &"a".repeat(53),
        &"a".repeat(54),
        &"界".repeat(60),
    ] {
        let wire = tool_name(original);
        assert!(format!("{PREFIX}{wire}").len() <= 64);
        assert!(wire
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte)));
    }
    assert_ne!(tool_name("a.b"), tool_name("a_b"));
    assert_eq!(tool_name(&"a".repeat(53)), "a".repeat(53));
    assert_eq!(tool_name(&"a".repeat(54)).len(), 53);
    let reserved = tool_name("a.b");
    assert_ne!(tool_name(&reserved), reserved);
}

#[test]
fn caller_tool_choice_filters_none_and_named_tools_and_rejects_unknown_names() {
    let base = json!({"tools":[{"name":"selected","input_schema":{"type":"object"}},{"name":"other","input_schema":{"type":"object"}}]});
    let mut request = base.clone();
    request["tool_choice"] = json!({"type":"none"});
    let (off, names) = tools(&request).unwrap();
    assert!(off.is_empty() && names.is_empty());
    request["tool_choice"] = json!({"type":"tool","name":"selected"});
    let (selected, names) = tools(&request).unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0]["name"], "selected");
    assert_eq!(names.len(), 1);
    assert!(tool_choice_ask(&request)
        .unwrap()
        .contains("mcp__nora__selected"));
    request["tool_choice"] = json!({"type":"tool","name":"missing"});
    assert!(tools(&request).is_err());
    request["tool_choice"] = json!({"type":"any","disable_parallel_tool_use":true});
    let ask = tool_choice_ask(&request).unwrap();
    assert!(
        ask.contains("at least one available tool") && ask.contains("at most one tool at a time")
    );
}
