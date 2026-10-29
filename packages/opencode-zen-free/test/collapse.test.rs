use super::*;
fn frames(values: &[Value], done: bool) -> BoxStream<'static, PluginResult<Frame>> {
    let mut frames: Vec<_> = values
        .iter()
        .map(|value| {
            Ok(Frame::json(
                value.clone(),
                value["type"].as_str().unwrap_or(""),
            ))
        })
        .collect();
    if done {
        frames.push(Ok(Frame {
            name: String::new(),
            text: "[DONE]".into(),
            lines: vec!["data: [DONE]".into()],
        }));
    }
    Box::pin(futures::stream::iter(frames))
}
#[tokio::test]
async fn chat_collapse_retains_multiple_choices_reasoning_tools_and_usage() {
    let values = [
        json!({"id":"reply","model":"native","choices":[{"index":1,"delta":{"role":"assistant","content":"second"},"finish_reason":"stop"},{"index":0,"delta":{"content":"first ","reasoning_content":"think","tool_calls":[{"index":0,"id":"call","type":"function","function":{"name":"read","arguments":"{}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":2}}),
    ];
    let value = collapse(frames(&values, true), Protocol::Chat)
        .await
        .unwrap();
    assert_eq!(value["choices"][0]["message"]["content"], "first ");
    assert_eq!(value["choices"][1]["message"]["content"], "second");
    assert_eq!(
        value["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "read"
    );
    assert_eq!(value["usage"]["prompt_tokens"], 10);
}
#[tokio::test]
async fn responses_collapse_prefers_the_real_final_snapshot_with_actual_usage() {
    let final_value = json!({"id":"reply","status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"whole"}]}],"usage":{"input_tokens":12,"output_tokens":4}});
    let values = [
        json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"partial"}),
        json!({"type":"response.completed","response":final_value.clone()}),
    ];
    assert_eq!(
        collapse(frames(&values, false), Protocol::Responses)
            .await
            .unwrap(),
        final_value
    );
}
#[tokio::test]
async fn anthropic_collapse_merges_input_output_cache_usage_and_parses_tool_arguments() {
    let values = [
        json!({"type":"message_start","message":{"id":"msg","content":[],"usage":{"input_tokens":8,"cache_read_input_tokens":2}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call","name":"read","input":{}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"a\"}"}}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":3}}),
        json!({"type":"message_stop"}),
    ];
    let value = collapse(frames(&values, false), Protocol::Anthropic)
        .await
        .unwrap();
    assert_eq!(value["content"][0]["input"]["path"], "a");
    assert_eq!(value["usage"]["input_tokens"], 8);
    assert_eq!(value["usage"]["output_tokens"], 3);
    assert_eq!(value["usage"]["cache_read_input_tokens"], 2);
}
#[tokio::test]
async fn unfinished_and_upstream_error_streams_never_turn_into_successful_json() {
    assert!(collapse(
        frames(
            &[json!({"choices":[{"index":0,"delta":{"content":"partial"}}]})],
            false
        ),
        Protocol::Chat
    )
    .await
    .is_err());
    assert!(collapse(
        frames(
            &[json!({"type":"error","error":{"message":"refused"}})],
            false
        ),
        Protocol::Responses
    )
    .await
    .is_err());
}

fn terminal(protocol: Protocol) -> Frame {
    match protocol {
        Protocol::Chat => Frame {
            name: String::new(),
            text: "[DONE]".into(),
            lines: vec!["data: [DONE]".into()],
        },
        Protocol::Responses => Frame::json(
            json!({"type":"response.completed","response":{"status":"completed","output":[]}}),
            "response.completed",
        ),
        Protocol::Anthropic => Frame::json(json!({"type":"message_stop"}), "message_stop"),
    }
}

#[tokio::test]
async fn upstream_errors_after_a_completion_marker_still_fail_every_protocol() {
    for protocol in [Protocol::Chat, Protocol::Responses, Protocol::Anthropic] {
        let source = Box::pin(futures::stream::iter(vec![
            Ok(terminal(protocol)),
            Ok(Frame::json(
                json!({"type":"error","error":{"message":"failure"}}),
                "error",
            )),
        ]));
        assert!(collapse(source, protocol).await.is_err());
    }
}

#[tokio::test]
async fn a_body_read_failure_after_completion_is_not_swallowed() {
    for protocol in [Protocol::Chat, Protocol::Responses, Protocol::Anthropic] {
        let source = Box::pin(futures::stream::iter(vec![
            Ok(terminal(protocol)),
            Err(PluginError::transport("fixture read failure")),
        ]));
        let error = collapse(source, protocol).await.unwrap_err();
        assert_eq!(error.message, "fixture read failure");
    }
}

#[tokio::test]
async fn an_incomplete_responses_snapshot_is_preserved_and_a_failed_one_is_never_success() {
    let response = json!({"id":"reply","status":"incomplete","output":[{"type":"message","content":[{"type":"output_text","text":"partial"}]}],"incomplete_details":{"reason":"max_output_tokens"},"usage":{"input_tokens":7,"output_tokens":2}});
    let source = frames(
        &[json!({"type":"response.incomplete","response":response.clone()})],
        false,
    );
    assert_eq!(
        collapse(source, Protocol::Responses).await.unwrap(),
        response
    );
    let source = frames(
        &[
            json!({"type":"response.failed","response":{"id":"reply","status":"failed","output":[]}}),
        ],
        false,
    );
    assert!(collapse(source, Protocol::Responses).await.is_err());
}

#[tokio::test]
async fn malformed_anthropic_tool_json_cannot_be_returned_as_a_finished_answer() {
    let source = frames(
        &[
            json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call","name":"read","input":{}}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}),
            json!({"type":"message_stop"}),
        ],
        false,
    );
    assert!(collapse(source, Protocol::Anthropic).await.is_err());
}
