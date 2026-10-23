use super::*;
use base64::Engine;
use nora_provider_plugin_sdk::FetchRequest;
use std::sync::Arc;

#[path = "x_search.test.rs"]
mod x_search;

async fn outgoing(body: &[u8], oauth: bool) -> nora_ai::HttpRequest {
    outgoing_with(body, oauth, "responses", None).await.unwrap()
}

async fn outgoing_with(
    body: &[u8],
    oauth: bool,
    path: &str,
    x_search: Option<Value>,
) -> PluginResult<nora_ai::HttpRequest> {
    let mut manifest: Value = serde_json::from_str(include_str!("../nora-provider.json")).unwrap();
    if let Some(value) = x_search {
        manifest["config"]["x_search"] = value;
    }
    let mut definition = Definition::parse(manifest).unwrap();
    definition.adapter = Arc::new(Xai);
    nora_provider::transport::build_request(
        &definition,
        FetchRequest {
            account: "test-account".into(),
            credentials: Some(if oauth {
                json!({"type":"oauth","credentialProvider":"xAI","access":"test-grok-access"})
            } else {
                json!({"type":"apiKey","access":"test-api-key"})
            }),
            network_proxy: None,
            url: format!(
                "{}/{path}",
                if oauth {
                    "https://cli-chat-proxy.grok.com/v1"
                } else {
                    "https://api.x.ai/v1"
                }
            ),
            method: "POST".into(),
            headers: BTreeMap::new(),
            metadata: BTreeMap::new(),
            body: Some(base64::engine::general_purpose::STANDARD.encode(body)),
            body_file: None,
        },
    )
    .await
}

async fn rewritten(payload: Value) -> Value {
    serde_json::from_slice(
        &outgoing(&serde_json::to_vec(&payload).unwrap(), true)
            .await
            .body
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn tool_choice_is_removed_when_no_supported_tools_remain() {
    for payload in [
        json!({"model":"grok-4.7","tool_choice":"none","input":[]}),
        json!({"model":"grok-4.7","tools":[],"tool_choice":"none","input":[]}),
        json!({"model":"grok-4.7","tool_choice":"auto","input":[]}),
        json!({"model":"grok-4.7","tools":[{"type":"custom","name":"apply_patch"}],"tool_choice":"none","input":[]}),
    ] {
        let sent = rewritten(payload).await;
        assert!(sent.get("tool_choice").is_none());
        assert_eq!(sent["model"], "grok-4.7");
        assert!(sent["tools"].as_array().is_none_or(Vec::is_empty));
    }
}

#[tokio::test]
async fn a_tool_choice_with_a_supported_function_keeps_the_original_body() {
    let body = br#"{ "tools": [{"type":"function","name":"x","parameters":{"type":"object"}}], "tool_choice":"none", "input":[] }"#;
    assert_eq!(outgoing(body, true).await.body.unwrap(), body);
}

#[tokio::test]
async fn a_plain_codex_task_becomes_a_user_message_without_requiring_tools() {
    let task = json!({"type":"agent_message","author":"/root","recipient":"/root/research","content":[{"type":"input_text","text":"Message Type: NEW_TASK\nPayload:\nresearch the failure"}]});
    let sent = rewritten(json!({"input":[task.clone()]})).await;
    assert_eq!(
        sent["input"],
        json!([{"type":"message","role":"user","content":[{"type":"input_text","text":"From /root to /root/research\n\n"},task["content"][0]]}])
    );
}

#[tokio::test]
async fn an_agent_reply_keeps_its_sender_recipient_position_and_multimodal_parts() {
    let before =
        json!({"type":"function_call","call_id":"call_1","name":"send_message","arguments":"{}"});
    let after = json!({"type":"function_call_output","call_id":"call_1","output":"sent"});
    let reply = json!({"type":"agent_message","author":"/root/research","recipient":"/root","content":[{"type":"input_text","text":"Message Type: FINAL_ANSWER\nPayload:\nfound the cause"},{"type":"input_image","image_url":"data:image/png;base64,fixture"}]});
    let sent =
        rewritten(json!({"tools":[],"input":[before.clone(),reply.clone(),after.clone()]})).await;
    assert_eq!(
        sent["input"],
        json!([before,{"type":"message","role":"user","content":[{"type":"input_text","text":"From /root/research to /root\n\n"},reply["content"][0],reply["content"][1]]},after])
    );
}

#[tokio::test]
async fn sealed_or_malformed_agent_messages_are_never_changed_or_dropped() {
    for item in [
        json!({"type":"agent_message","content":[{"type":"encrypted_content","encrypted_content":"sealed fixture"}]}),
        json!({"type":"agent_message","content":[{"type":"input_text","text":"header"},{"type":"encrypted_content","encrypted_content":"sealed fixture"}]}),
        json!({"type":"agent_message","content":[{"type":"input_text","encrypted_content":"sealed fixture"}]}),
        json!({"type":"agent_message","encrypted_content":"sealed fixture","content":[{"type":"input_text","text":"header"}]}),
        json!({"type":"agent_message","content":[{"type":"future_content","payload":"keep me"}]}),
        json!({"type":"agent_message","content":[]}),
        json!({"type":"agent_message","content":null}),
    ] {
        let body = serde_json::to_vec(&json!({"input":[item]})).unwrap();
        assert_eq!(outgoing(&body, true).await.body.unwrap(), body);
    }
}

#[tokio::test]
async fn ordinary_responses_histories_keep_the_original_body() {
    let body = br#"{ "input": [{"type":"message","role":"user","content":"hello"}] }"#;
    assert_eq!(outgoing(body, true).await.body.unwrap(), body);
}

#[tokio::test]
async fn root_tool_unions_keep_nested_branch_fields_and_only_common_requirements() {
    let parameters: Value =
        serde_json::from_str(include_str!("fixtures/codex_automation_update_schema.json")).unwrap();
    let plain_tool = json!({"type":"function","name":"exec_command","parameters":{"type":"object","properties":{"cmd":{"type":"string"}}}});
    let sent = rewritten(json!({"model":"grok-4.7","tools":[plain_tool.clone(),{"type":"function","name":"mcp__codex_app__automation_update","strict":false,"parameters":parameters}]})).await;
    let parameters = &sent["tools"][1]["parameters"];
    assert_eq!(parameters["type"], "object");
    assert!(parameters.get("anyOf").is_none());
    assert!(parameters.get("oneOf").is_none());
    for key in [
        "mode",
        "id",
        "name",
        "prompt",
        "rrule",
        "status",
        "kind",
        "projectId",
        "model",
        "targetThreadId",
        "executionEnvironment",
    ] {
        assert!(parameters["properties"].get(key).is_some(), "missing {key}");
    }
    assert_eq!(parameters["required"], json!(["mode"]));
    let modes = parameters["properties"]["mode"].to_string();
    for mode in ["view", "delete", "__schema11"] {
        assert!(modes.contains(mode), "missing {mode}");
    }
    assert_eq!(sent["tools"][0], plain_tool);
}

#[tokio::test]
async fn an_object_schema_with_nested_unions_keeps_the_original_body() {
    let body = br#"{ "tools": [{"type":"function","name":"test","parameters":{"type":"object","properties":{"a":{"anyOf":[{"type":"string"},{"type":"null"}]}}}}] }"#;
    assert_eq!(outgoing(body, true).await.body.unwrap(), body);
}

#[tokio::test]
async fn chat_function_root_parameters_are_folded_as_well() {
    let sent = rewritten(json!({"tools":[{"type":"function","function":{"name":"t","parameters":{"anyOf":[{"type":"object","properties":{"a":{"type":"string"}}},{"type":"string"}]}}}]})).await;
    assert_eq!(
        sent["tools"][0]["function"]["parameters"],
        json!({"type":"object","properties":{"a":{"type":"string"}}})
    );
}

#[tokio::test]
async fn local_schema_references_and_all_of_requirements_survive_root_union_folding() {
    let parameters = json!({
        "$defs":{"first":{"type":"object","properties":{"common":{"type":"string"},"left":{"type":"number"}},"required":["common","left"]}},
        "definitions":{"second":{"type":"object","properties":{"common":{"type":"string"},"right":{"type":"boolean"}},"required":["common","right"]}},
        "properties":{"root":{"const":"kept"}},"required":["root"],
        "allOf":[{"type":"object","properties":{"always":{"type":"string"}},"required":["always"]}],
        "oneOf":[{"$ref":"#/$defs/first"},{"$ref":"#/definitions/second"},{"type":"null"}]
    });
    let sent =
        rewritten(json!({"tools":[{"type":"function","name":"test","parameters":parameters}]}))
            .await;
    let schema = &sent["tools"][0]["parameters"];
    assert_eq!(schema["required"], json!(["root", "always", "common"]));
    for key in ["root", "always", "common", "left", "right"] {
        assert!(schema["properties"].get(key).is_some(), "missing {key}");
    }
    assert!(schema.get("allOf").is_none());
    assert!(schema.get("oneOf").is_none());
    assert!(schema.get("$defs").is_some());
    assert!(schema.get("definitions").is_some());
}

#[tokio::test]
async fn api_key_requests_do_not_apply_subscription_compatibility_changes() {
    let body = br#"{ "tools":[], "tool_choice":"none", "input":[{"type":"agent_message","content":[{"type":"input_text","text":"hello"}]}] }"#;
    assert_eq!(outgoing(body, false).await.body.unwrap(), body);
}

#[tokio::test]
async fn unsupported_tool_choices_search_flags_and_null_reasoning_are_removed_together() {
    let sent = rewritten(json!({
        "tools":[{"type":"web_search","external_web_access":true},{"type":"namespace","name":"agents"}],
        "tool_choice":{"type":"custom","name":"apply_patch"},
        "input":[{"type":"reasoning","content":null,"encrypted_content":"sealed reasoning"}]
    })).await;
    assert_eq!(sent["tools"], json!([{"type":"web_search"}]));
    assert!(sent.get("tool_choice").is_none());
    assert_eq!(
        sent["input"],
        json!([{"type":"reasoning","encrypted_content":"sealed reasoning"}])
    );
}

#[tokio::test]
async fn malformed_array_tools_and_choices_follow_the_reference_filter() {
    let sent = rewritten(json!({
        "tools":[[],{"type":"function","name":"x","parameters":{"type":"object"}}],
        "tool_choice":[]
    }))
    .await;
    assert_eq!(
        sent["tools"],
        json!([{"type":"function","name":"x","parameters":{"type":"object"}}])
    );
    assert!(sent.get("tool_choice").is_none());
}
