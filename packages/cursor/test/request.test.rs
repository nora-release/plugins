use super::*;
#[test]
fn exact_caller_tools_and_original_schema_are_the_only_mcp_catalog() {
    let chat = crate::fixture::chat();
    let catalog = tools(&chat).unwrap();
    let conv = conversation(&chat, &catalog).unwrap();
    let system: Value = serde_json::from_slice(&conv.messages[0]).unwrap();
    assert!(system["content"]
        .as_str()
        .unwrap()
        .contains("CallDynamicTool"));
    assert!(system["content"]
        .as_str()
        .unwrap()
        .contains("\"value\":{\"type\":\"string\"}"));
    let mut chat = chat;
    chat["tool_choice"] = json!("none");
    assert!(tools(&chat).unwrap().is_empty());
    chat["tool_choice"] = json!({"type":"function","function":{"name":"missing"}});
    assert!(tools(&chat).is_err());
}
#[test]
fn split_parallel_calls_and_error_results_preserve_pairing() {
    let chat = json!({"messages":[{"role":"user","content":"run both"},{"role":"assistant","content":"","tool_calls":[{"id":"a","function":{"name":"echo","arguments":"{\"value\":1}"}}]},{"role":"assistant","reasoning_content":"thought","tool_calls":[{"id":"b","function":{"name":"echo","arguments":"{\"value\":2}"}}]},{"role":"tool","tool_call_id":"b","content":"failure","is_error":true},{"role":"tool","tool_call_id":"a","content":"ok"}]});
    let conv = conversation(&chat, &[]).unwrap();
    let rows: Vec<Value> = conv
        .messages
        .iter()
        .map(|m| serde_json::from_slice(m).unwrap())
        .collect();
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows[1]["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["type"] == "tool-call")
            .count(),
        2
    );
    assert_eq!(rows[2]["content"][0]["toolCallId"], "b");
    assert_eq!(rows[2]["content"][0]["isError"], true);
    assert_eq!(rows[2]["content"][1]["toolCallId"], "a");
}
#[test]
fn unsupported_files_and_remote_images_fail_without_downloading_or_substituting_text() {
    for content in [
        json!([{"type":"file","file":{"filename":"secret.pdf","file_data":"data:application/pdf;base64,c2VjcmV0"}}]),
        json!([{"type":"image_url","image_url":{"url":"file:///etc/passwd"}}]),
        json!([{"type":"image_url","image_url":{"url":"https://example.test/private.png"}}]),
        json!([{"type":"image_url","image_url":{"url":"data:image/png;base64,not!base64"}}]),
    ] {
        let error = conversation(
            &json!({"messages":[{"role":"user","content":content}]}),
            &[],
        )
        .err()
        .unwrap();
        assert_eq!(error.code, "unsupported_media_type");
        assert!(!error.message.contains("secret.pdf"));
    }
}
#[test]
fn native_results_preserve_images_text_and_error_flag() {
    let bytes=native_result(&json!({"content":[{"type":"text","text":"done"},{"type":"image_url","image_url":{"url":"data:image/png;base64,aGk="}}],"is_error":true})).unwrap();
    let fields = protobuf::fields(&bytes).unwrap();
    let success = protobuf::fields(protobuf::field(&fields, 1).unwrap().data).unwrap();
    assert_eq!(protobuf::num(&success, 2).unwrap(), 1);
    assert_eq!(success.iter().filter(|f| f.id == 1).count(), 2);
}
#[test]
fn run_bootstraps_content_addressed_blobs_and_exposes_no_local_shell_capability() {
    let chat = crate::fixture::chat();
    let tools = tools(&chat).unwrap();
    let conv = conversation(&chat, &tools).unwrap();
    let selection = Selection {
        id: "claude-medium".into(),
        params: BTreeMap::from([("effort".into(), "medium".into())]),
        max_mode: true,
    };
    let (first, blobs) = build(&conv, &tools, &selection, "conversation").unwrap();
    for (key, value) in &blobs {
        assert_eq!(
            auth::hash(value),
            key.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
    }
    let top = protobuf::fields(&first).unwrap();
    let run = protobuf::fields(protobuf::field(&top, 1).unwrap().data).unwrap();
    assert_eq!(protobuf::string(&run, 5).unwrap(), "conversation");
    assert_eq!(protobuf::num(&run, 19).unwrap(), 1);
    let requested = protobuf::fields(protobuf::field(&run, 9).unwrap().data).unwrap();
    assert_eq!(protobuf::num(&requested, 2).unwrap(), 1);
}
