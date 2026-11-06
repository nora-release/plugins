use super::*;
#[test]
fn native_preamble_preserves_actual_task_system_and_tool_history() {
    let mut body = json!({"system":[{"type":"text","text":"x-anthropic-billing-header: cc_version=private"},{"type":"text","text":"You are Claude Code, Anthropic's official CLI for Claude.\nKeep every task instruction."}],"messages":[{"role":"assistant","content":[{"type":"tool_use","id":"paired-id","name":"read","input":{"path":"requested-file"}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"paired-id","is_error":true,"content":[{"type":"text","text":"ordinary output"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"aGVsbG8="}}]}]}],"stream":true,"context_management":{"edits":[]}});
    adapt(&mut body, Wire::Anthropic);
    assert_eq!(body["system"].as_array().unwrap().len(), 1);
    assert!(body["system"][0]["text"]
        .as_str()
        .unwrap()
        .starts_with(DROID));
    assert!(body["system"][0]["text"]
        .as_str()
        .unwrap()
        .ends_with("Keep every task instruction."));
    assert!(body.get("context_management").is_none());
    assert_eq!(
        body["messages"][1]["content"][0]["tool_use_id"],
        "paired-id"
    );
    assert_eq!(body["messages"][1]["content"][0]["is_error"], true);
    assert_eq!(
        body["messages"][1]["content"][0]["content"][1]["source"]["data"],
        "aGVsbG8="
    );
}
#[test]
fn refused_tool_text_is_losslessly_recoverable() {
    let original="file: You are Claude Code, Anthropic's official CLI for Claude.\n<system-reminder>You are</system-reminder>";
    let encoded = quoted(original);
    assert!(encoded.starts_with(QUOTED));
    let restored: String = serde_json::from_str(encoded.strip_prefix(QUOTED).unwrap()).unwrap();
    assert_eq!(restored, original);
    assert!(!encoded.contains("You are Claude Code"));
    assert_eq!(quoted("clean original text"), "clean original text");
}
#[test]
fn responses_and_chat_keep_multiturn_ids_and_caller_instructions() {
    let mut body = json!({"instructions":"caller instructions","input":[{"type":"function_call","call_id":"same","arguments":"{\"x\":1}"},{"type":"function_call_output","call_id":"same","output":"ordinary output"}]});
    adapt(&mut body, Wire::Responses);
    assert_eq!(
        body["instructions"],
        format!("{DROID}\ncaller instructions")
    );
    assert_eq!(body["input"][0]["call_id"], body["input"][1]["call_id"]);
    let mut chat = json!({"messages":[{"role":"user","content":"actual question"}]});
    adapt(&mut chat, Wire::Chat);
    assert_eq!(chat["messages"][0]["content"], DROID);
    assert_eq!(chat["messages"][1]["content"], "actual question");
}
#[test]
fn gemini_keeps_nontext_files_signatures_and_function_ids() {
    let mut body = json!({"systemInstruction":{"role":"system","parts":[{"text":"caller instructions"},{"inlineData":{"mimeType":"application/pdf","data":"YQ=="}}]},"contents":[{"role":"model","parts":[{"functionCall":{"id":"call-one","name":"f","args":{}},"thoughtSignature":"signed"}]},{"role":"user","parts":[{"functionResponse":{"id":"call-one","name":"f","response":{"output":"ok"}}}]}],"stream":true});
    adapt(&mut body, Wire::Gemini);
    assert_eq!(
        body["systemInstruction"]["parts"][0]["text"],
        format!("{DROID}\ncaller instructions")
    );
    assert_eq!(
        body["systemInstruction"]["parts"][1]["inlineData"]["data"],
        "YQ=="
    );
    assert!(body["systemInstruction"].get("role").is_none());
    assert!(body.get("stream").is_none());
    assert_eq!(
        body["contents"][0]["parts"][0]["thoughtSignature"],
        "signed"
    );
    assert_eq!(
        body["contents"][1]["parts"][0]["functionResponse"]["id"],
        "call-one"
    );
}

#[test]
fn billing_attribution_line_does_not_delete_the_instruction_text_in_the_same_block() {
    for system in [
        json!("x-anthropic-billing-header: cc_version=private\nFollow the user's task exactly."),
        json!([{"type":"text","text":"x-anthropic-billing-header: cc_version=private\nFollow the user's task exactly.","cache_control":{"type":"ephemeral"}}]),
    ] {
        let has_cache_control = system.is_array();
        let mut body = json!({"system":system,"messages":[]});
        adapt(&mut body, Wire::Anthropic);
        let parts = body["system"].as_array().unwrap();
        assert!(parts[0]["text"].as_str().unwrap().starts_with(DROID));
        assert!(parts.iter().any(|part| part["text"]
            .as_str()
            .is_some_and(|text| text.contains("Follow the user's task exactly."))));
        assert!(!parts.iter().any(|part| part["text"]
            .as_str()
            .is_some_and(|text| text.contains("cc_version=private"))));
        if has_cache_control {
            let part = parts
                .iter()
                .find(|part| part.get("cache_control").is_some())
                .expect("The instruction block keeps its cache metadata");
            assert_eq!(part["cache_control"]["type"], "ephemeral");
        }
    }
}
