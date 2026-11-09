use super::*;
#[test]
fn split_assistant_tool_calls_are_paired_with_one_user_result_for_each_call() {
    let req = json!({"messages":[{"role":"user","content":"start"},{"role":"assistant","content":[{"type":"tool_use","id":"a","name":"mcp/tool/one","input":{"x":1}}]},{"role":"assistant","content":[{"type":"tool_use","id":"b","name":"second","input":{}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"a","content":"done"},{"type":"tool_result","tool_use_id":"a","content":"duplicate"},{"type":"tool_result","tool_use_id":"orphan","content":"wrong"}]}]});
    let (body, names) = build(&req, "auto", "profile", 0).unwrap();
    let history = body.pointer("/conversationState/history").unwrap();
    assert_eq!(history.as_array().unwrap().len(), 2);
    assert_eq!(
        history[1]["assistantResponseMessage"]["toolUses"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let current = &body["conversationState"]["currentMessage"]["userInputMessage"];
    let results = current["userInputMessageContext"]["toolResults"]
        .as_array()
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["content"][0]["text"], "done");
    assert_eq!(results[1]["status"], "error");
    assert_eq!(
        current["userInputMessageContext"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(names.values().any(|name| name == "mcp/tool/one"));
}
#[test]
fn tool_identifiers_and_long_names_are_stable_and_valid() {
    for original in ["", &"long/工具".repeat(30)] {
        let wire = name(original);
        assert!(wire.len() <= 64);
        assert!(wire
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte)));
        assert_eq!(wire, name(original));
        let wire = id(original);
        assert!(wire.len() <= 64);
        assert_eq!(wire, id(original));
    }
    assert_eq!(id("existing:1.2"), "existing:1.2");
}
#[test]
fn tool_images_follow_the_latest_user_message_and_documents_become_text() {
    let image = json!({"type":"image","source":{"type":"base64","media_type":"image/jpeg","data":"aW1hZ2U="}});
    let req = json!({"system":[{"type":"text","text":"Be exact."}],"messages":[{"role":"user","content":[image.clone(),{"type":"text","text":"start"}]},{"role":"assistant","content":[{"type":"tool_use","id":"call","name":"look","input":{}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"call","content":[image,{"type":"text","text":"screenshot"}]},{"type":"document","title":"Document","source":{"type":"text","data":"document contents"}}]}]});
    let (body, _) = build(&req, "claude-x", "", 10000).unwrap();
    let state = &body["conversationState"];
    assert!(state["history"][0]["userInputMessage"]
        .get("images")
        .is_none());
    let user = &state["currentMessage"]["userInputMessage"];
    assert_eq!(user["images"][0]["source"]["bytes"], "aW1hZ2U=");
    assert!(user["content"]
        .as_str()
        .unwrap()
        .contains("document contents"));
    assert!(state["history"][0]["userInputMessage"]["content"]
        .as_str()
        .unwrap()
        .contains("<max_thinking_length>10000"));
}
#[test]
fn source_messages_without_a_matching_call_are_removed_and_empty_request_gets_a_prompt() {
    let(body,_)=build(&json!({"messages":[{"role":"assistant","content":"orphan"},{"role":"user","content":[{"type":"tool_result","tool_use_id":"orphan","content":"discard"}]}]}),"auto","",0).unwrap();
    assert!(body["conversationState"].get("history").is_none());
    assert_eq!(
        body.pointer("/conversationState/currentMessage/userInputMessage/content")
            .unwrap(),
        "Please proceed with the task."
    );
    let (body, _) = build(&json!({"messages":[]}), "auto", "", 0).unwrap();
    assert_eq!(
        body.pointer("/conversationState/currentMessage/userInputMessage/content")
            .unwrap(),
        "Please proceed with the task."
    );
}
#[test]
fn cache_points_are_limited_to_the_latest_four_across_tools_and_messages() {
    let mut messages = Vec::new();
    for i in 0..6 {
        messages.push(json!({"role":if i%2==0{"user"}else{"assistant"},"content":[{"type":"text","text":"hi","cache_control":{"type":"ephemeral"}}]}));
    }
    let req = json!({"messages":messages,"tools":[{"name":"a","cache_control":{"type":"ephemeral"}},{"name":"b","cache_control":{"type":"ephemeral"}}]});
    let (body, _) = build(&req, "auto", "", 0).unwrap();
    assert_eq!(body.to_string().matches("\"cachePoint\"").count(), 4);
}
#[test]
fn metadata_effort_uses_kiro_budgets_and_does_not_enable_nonclaude_models() {
    assert_eq!(thinking(&json!({}), "auto", Some("high")), 30000);
    assert_eq!(
        thinking(
            &json!({"thinking":{"type":"enabled"}}),
            "claude-x",
            Some("low")
        ),
        10000
    );
    assert_eq!(
        thinking(
            &json!({"thinking":{"type":"disabled"}}),
            "claude-x",
            Some("high")
        ),
        0
    );
    assert_eq!(thinking(&json!({}), "other", Some("high")), 0);
    assert_eq!(
        thinking(
            &json!({"thinking":{"type":"enabled","budget_tokens":1234}}),
            "auto",
            None
        ),
        1234
    );
}
#[test]
fn structured_output_and_history_tools_are_described_to_kiro() {
    let(body,_)=build(&json!({"output_config":{"format":{"type":"json_schema","schema":{"type":"object","required":["answer"]}}},"messages":[{"role":"user","content":"hello"}],"tools":[{"name":"known","input_schema":{"type":"object","properties":{"q":{"type":"string"}}}}]}),"auto","profile",0).unwrap();
    let current = &body["conversationState"]["currentMessage"]["userInputMessage"];
    assert!(current["content"]
        .as_str()
        .unwrap()
        .contains("single JSON value"));
    assert_eq!(
        current["userInputMessageContext"]["tools"][0]["toolSpecification"]["inputSchema"]["json"]
            ["properties"]["q"]["type"],
        "string"
    );
    assert_eq!(body["profileArn"], "profile");
}
