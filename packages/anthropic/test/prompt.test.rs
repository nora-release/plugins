use super::*;
#[test]
fn full_history_system_images_and_tool_names_are_rendered_without_taking_old_turns_as_current() {
    let request = json!({"system":"Respect the current user.","messages":[{"role":"user","content":"old question"},{"role":"assistant","content":"old answer"},{"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"aW1hZ2U="}},{"type":"text","text":"new question"}]}]});
    let blocks = initial(&request).unwrap();
    let text = json!(blocks).to_string();
    assert!(text.contains("external_system_instructions"));
    assert!(text.contains("previous_conversation"));
    assert!(text.contains("current_turn"));
    assert!(text.contains("old answer"));
    assert!(text.contains("aW1hZ2U="));
    assert!(text.contains("new question"));
    assert_eq!(blocks[0]["cache_control"]["ttl"], "1h");
}
#[test]
fn cache_markers_and_thinking_do_not_change_conversation_ownership_but_actual_tool_ids_do() {
    let first = vec![
        json!({"role":"assistant","content":[{"type":"thinking","thinking":"private thought"},{"type":"tool_use","id":"a","name":"tool","input":{"x":1},"cache_control":{"type":"ephemeral"}}]}),
    ];
    let second = vec![
        json!({"role":"assistant","content":[{"type":"tool_use","id":"a","name":"tool","input":{"x":1}}]}),
    ];
    assert_eq!(canonical(&first), canonical(&second));
    let different = vec![
        json!({"role":"assistant","content":[{"type":"tool_use","id":"b","name":"tool","input":{"x":1}}]}),
    ];
    assert_ne!(canonical(&first), canonical(&different));
}
#[test]
fn duplicate_result_ids_are_rejected_and_results_keep_image_bytes() {
    let message = json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"a","content":[{"type":"text","text":"done"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"bytes"}}]}]});
    let result = results(&[message.clone()]).unwrap();
    assert_eq!(result[0].1["content"][1]["data"], "bytes");
    assert!(results(&[message.clone(), message]).is_err());
}

#[test]
fn searched_history_can_restart_without_replaying_opaque_provider_content() {
    let history = json!({"role":"assistant","content":[
        {"type":"server_tool_use","id":"srvtoolu_previous","name":"web_search","input":{"query":"official documentation","private":"must-not-copy"}},
        {"type":"web_search_tool_result","tool_use_id":"srvtoolu_previous","content":[
            {"type":"web_search_result","title":"Documentation","url":"https://example.com/docs","page_age":"today","encrypted_content":"must-not-copy","private":"must-not-copy"},
            {"type":"web_search_result","title":"Private","url":"https://user:password@example.com/private","encrypted_content":"must-not-copy"}
        ]},
        {"type":"text","text":"Previous public answer."}
    ]});
    let request = json!({"messages":[{"role":"user","content":"past question"},history,{"role":"user","content":"Continue with the public source."}]});
    let initial = json!(initial(&request).unwrap()).to_string();
    let turn = json!(turn(&request["messages"].as_array().unwrap()[1..]).unwrap()).to_string();
    for text in [initial, turn] {
        assert!(text.contains("previous_web_search"));
        assert!(text.contains("official documentation"));
        assert!(text.contains("https://example.com/docs") && text.contains("today"));
        assert!(text.contains("Continue with the public source."));
        for secret in [
            "must-not-copy",
            "encrypted_content",
            "password",
            "srvtoolu_previous",
        ] {
            assert!(!text.contains(secret));
        }
    }
}

#[test]
fn replay_preserves_public_search_errors_and_rejects_unhandled_server_tools() {
    let history = vec![
        json!({"role":"assistant","content":[{"type":"web_search_tool_result","tool_use_id":"srv-search","content":{"type":"web_search_tool_result_error","error_code":"max_uses_exceeded","private":"must-not-copy"}}]}),
    ];
    let text = json!(turn(&history).unwrap()).to_string();
    assert!(text.contains("max_uses_exceeded") && !text.contains("must-not-copy"));
    assert!(turn(&[json!({"role":"assistant","content":[{"type":"server_tool_use","name":"code_execution","input":{"code":"private"}}]})]).is_err());
}
