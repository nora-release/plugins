use super::*;

fn function() -> Value {
    json!({"type":"function","name":"read_file","parameters":{"type":"object","properties":{}}})
}

async fn enabled(value: Value) -> Value {
    serde_json::from_slice(
        &outgoing_with(
            &serde_json::to_vec(&value).unwrap(),
            true,
            "responses",
            Some(json!(true)),
        )
        .await
        .unwrap()
        .body
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn native_x_search_is_an_opt_in_plugin_setting() {
    let manifest: Value = serde_json::from_str(include_str!("../nora-plugin.json")).unwrap();
    let option = manifest["contributes"]["provider"]["config"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|option| option["id"] == "x_search")
        .unwrap();
    assert_eq!(option["type"], "boolean");
    assert_eq!(option["label"], "Native X search");
    assert_eq!(option["default"], false);
    assert!(option["description"].as_str().unwrap().len() <= 300);
}

#[tokio::test]
async fn automatic_tools_gain_search_and_preserve_the_original_choice() {
    for choice in [None, Some(Value::Null), Some(json!("auto"))] {
        let mut body = json!({"tools":[function()],"input":"hello"});
        if let Some(choice) = &choice {
            body["tool_choice"] = choice.clone();
        }
        let sent = enabled(body).await;
        assert_eq!(sent["tools"], json!([function(),{"type":"x_search"}]));
        assert_eq!(sent.get("tool_choice"), choice.as_ref());
        assert_eq!(sent["input"], "hello");
    }
}

#[tokio::test]
async fn explicit_auto_can_enable_search_without_declared_tools() {
    for tools in [None, Some(Value::Null), Some(json!([]))] {
        let mut body = json!({"tool_choice":"auto"});
        if let Some(tools) = tools {
            body["tools"] = tools;
        }
        let sent = enabled(body).await;
        assert_eq!(sent["tools"], json!([{"type":"x_search"}]));
        assert_eq!(sent["tool_choice"], "auto");
    }
}

#[tokio::test]
async fn existing_search_filters_keep_the_original_body_without_duplicate_tools() {
    let body = serde_json::to_vec(&json!({"tools":[function(),{"type":"x_search","allowed_x_handles":["OpenAI"],"from_date":"2026-10-01","to_date":"2026-10-20","enable_image_understanding":true}],"tool_choice":"auto"})).unwrap();
    assert_eq!(
        outgoing_with(&body, true, "responses", Some(json!(true)))
            .await
            .unwrap()
            .body
            .unwrap(),
        body
    );
}

#[tokio::test]
async fn explicit_tool_choices_never_gain_search_even_when_unsupported_choices_are_removed() {
    for choice in [
        json!("none"),
        json!("required"),
        json!({"type":"function","name":"read_file"}),
        json!({"type":"allowed_tools","mode":"auto","tools":[{"type":"function","name":"read_file"}]}),
        json!({"type":"x_search"}),
    ] {
        let sent = enabled(json!({"tools":[function()],"tool_choice":choice.clone()})).await;
        assert_eq!(sent["tools"], json!([function()]));
        if choice["type"] == "allowed_tools" {
            assert!(sent.get("tool_choice").is_none());
        } else {
            assert_eq!(sent["tool_choice"], choice);
        }
    }
}

#[tokio::test]
async fn disabled_or_missing_tools_do_not_silently_enable_search() {
    for body in [
        json!({"input":"hi"}),
        json!({"tools":[]}),
        json!({"tools":null}),
        json!({"tool_choice":null}),
        json!({"tools":[],"tool_choice":null}),
        json!({"tools":[{"type":"custom","name":"apply_patch"}]}),
        json!({"tool_choice":"none"}),
        json!({"tools":[],"tool_choice":"none"}),
        json!({"tools":[{"type":"custom","name":"apply_patch"}],"tool_choice":"none"}),
    ] {
        let sent = enabled(body).await;
        assert!(sent["tools"].is_null() || sent["tools"] == json!([]));
        assert!(sent.get("tool_choice").is_none());
    }
}

#[tokio::test]
async fn malformed_requests_and_non_array_tools_are_never_given_search() {
    for body in [
        b"not JSON".as_slice(),
        b"null",
        b"[]",
        b"42",
        br#"{"tools":{}}"#,
        br#"{"tools":"invalid","tool_choice":"auto"}"#,
    ] {
        if let Ok(sent) = outgoing_with(body, true, "responses", Some(json!(true))).await {
            assert!(!String::from_utf8(sent.body.unwrap())
                .unwrap()
                .contains("x_search"));
        }
    }
}

#[tokio::test]
async fn live_switch_changes_apply_only_to_subscription_responses_requests() {
    let body = serde_json::to_vec(&json!({"tools":[function()]})).unwrap();
    for (setting, oauth, path, expected) in [
        (None, true, "responses", false),
        (Some(json!(true)), true, "responses", true),
        (Some(json!(true)), true, "other", false),
        (Some(json!(false)), true, "responses", false),
        (Some(json!(true)), false, "responses", false),
    ] {
        let request = outgoing_with(&body, oauth, path, setting).await.unwrap();
        let sent: Value = serde_json::from_slice(&request.body.unwrap()).unwrap();
        assert_eq!(
            sent["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["type"] == "x_search"),
            expected
        );
        assert_eq!(
            request
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
                .unwrap()
                .1,
            if oauth {
                "Bearer test-grok-access"
            } else {
                "Bearer test-api-key"
            }
        );
    }
}

#[tokio::test]
async fn search_injection_preserves_schema_reasoning_and_model_window_conversion() {
    for stream in [false, true] {
        for model in ["grok-4.6", "grok-4.7", "grok-4.7@500k", "future-grok"] {
            let sent = enabled(json!({"model":model,"stream":stream,"tools":[{"type":"custom","name":"apply_patch"},{"type":"function","name":"read_file","parameters":{"anyOf":[{"type":"object","properties":{"path":{"type":"string"}}},{"type":"string"}]}}],"tool_choice":"auto","input":[{"type":"reasoning","encrypted_content":"fixture","content":null}]})).await;
            assert_eq!(
                sent["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|tool| tool["type"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                vec!["function", "x_search"]
            );
            assert_eq!(
                sent["tools"][0]["parameters"],
                json!({"type":"object","properties":{"path":{"type":"string"}}})
            );
            assert_eq!(
                sent["input"],
                json!([{"type":"reasoning","encrypted_content":"fixture"}])
            );
            assert_eq!(sent["stream"], stream);
            assert_eq!(sent["model"], model.split('@').next().unwrap());
        }
    }
}
