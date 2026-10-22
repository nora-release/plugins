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
