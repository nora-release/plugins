use super::*;
#[test]
fn anthropic_whole_preserves_original_tool_ids_arguments_and_both_usage_counts() {
    let events = crate::fixture::anthropic()
        .into_iter()
        .filter_map(|v| v.get("event").cloned())
        .collect::<Vec<_>>();
    let value = assemble(Wire::Anthropic, &events).unwrap();
    assert_eq!(value["content"][0]["id"], "original-tool-id");
    assert_eq!(value["content"][0]["name"], "original_name");
    assert_eq!(value["content"][0]["input"], json!({"x":1}));
    assert_eq!(value["usage"], json!({"input_tokens":12,"output_tokens":2}));
    assert_eq!(value["stop_reason"], "tool_use");
}
#[test]
fn anthropic_thinking_signature_fragments_are_preserved() {
    let events = vec![
        json!({"type":"message_start","message":{"type":"message","usage":{"input_tokens":1}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"thought"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig-a"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig-b"}}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),
        json!({"type":"message_stop"}),
    ];
    let value = assemble(Wire::Anthropic, &events).unwrap();
    assert_eq!(value["content"][0]["thinking"], "thought");
    assert_eq!(value["content"][0]["signature"], "sig-asig-b");
}
#[test]
fn incomplete_tool_json_and_unfinished_chat_are_rejected() {
    let mut events = crate::fixture::anthropic()
        .into_iter()
        .filter_map(|v| v.get("event").cloned())
        .collect::<Vec<_>>();
    events[3]["delta"]["partial_json"] = json!("broken");
    assert!(assemble(Wire::Anthropic, &events).is_err());
    assert!(assemble(Wire::Chat,&[json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call","function":{"name":"f","arguments":"{"}}]}}]})]).is_err());
}
#[test]
fn responses_keeps_the_server_final_object_including_function_ids_and_incomplete_status() {
    let response = json!({"id":"r","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[{"type":"function_call","id":"item-id","call_id":"call-id","name":"f","arguments":"{}"}],"usage":{"input_tokens":10,"output_tokens":2}});
    let result = assemble(
        Wire::Responses,
        &[json!({"type":"response.incomplete","response":response})],
    )
    .unwrap();
    assert_eq!(result, response);
}
#[test]
fn gemini_keeps_empty_signed_parts_and_parallel_function_ids() {
    let events = vec![
        json!({"candidates":[{"content":{"parts":[{"text":"","thoughtSignature":"signed-empty"},{"text":"hello"}]}}]}),
        json!({"candidates":[{"content":{"parts":[{"functionCall":{"id":"a","name":"one","args":{"x":1}}},{"functionCall":{"id":"b","name":"two","args":{"y":2}}}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":4,"candidatesTokenCount":2}}),
    ];
    let value = assemble(Wire::Gemini, &events).unwrap();
    let parts = &value["candidates"][0]["content"]["parts"];
    assert_eq!(parts[0]["thoughtSignature"], "signed-empty");
    assert_eq!(parts[2]["functionCall"]["id"], "a");
    assert_eq!(parts[3]["functionCall"]["id"], "b");
    assert_eq!(value["usageMetadata"]["promptTokenCount"], 4);
}
