use super::*;
fn names() -> Names {
    Names {
        declared: BTreeSet::from(["read".into(), "todo_write".into(), "web_fetch".into()]),
    }
}
fn body(values: &[Value], done: bool) -> nora_ai::HttpByteStream {
    let mut bytes = Vec::new();
    for value in values {
        bytes.extend(Frame::json(value.clone(), value["type"].as_str().unwrap_or("")).encoded());
    }
    if done {
        bytes.extend_from_slice(b"data: [DONE]\n\n");
    }
    let chunks: Vec<_> = bytes
        .chunks(7)
        .map(|byte| Ok(byte.to_vec().into()))
        .collect();
    Box::pin(futures::stream::iter(chunks))
}
#[test]
fn tools_add_only_missing_real_parameter_standins_and_keep_the_original_policy() {
    for protocol in [Protocol::Chat, Protocol::Responses, Protocol::Anthropic] {
        let mut body = json!({"tools":[]});
        let policy = normalize(&mut body, protocol).unwrap();
        assert_eq!(body["tools"].as_array().unwrap().len(), 5);
        assert!(policy.declared.is_empty());
        let tool = &body["tools"][0];
        let definition = if protocol == Protocol::Chat {
            &tool["function"]
        } else {
            tool
        };
        let parameters = if protocol == Protocol::Anthropic {
            &definition["input_schema"]
        } else {
            &definition["parameters"]
        };
        assert_eq!(parameters["properties"]["command"]["type"], "string");
        assert!(parameters.get("required").is_none());
    }
    assert!(normalize(&mut json!({"tools":{}}), Protocol::Chat).is_err());
}
#[test]
fn unique_case_and_separator_matches_are_repaired_but_unknown_and_ambiguous_names_are_preserved() {
    let policy = names();
    assert_eq!(
        policy.check(&json!("ToDoWrite")).as_deref(),
        Some("todo_write")
    );
    assert_eq!(
        policy.check(&json!("WEBFETCH")).as_deref(),
        Some("web_fetch")
    );
    assert_eq!(policy.check(&json!("unknown")).as_deref(), Some("unknown"));
    let policy = Names {
        declared: BTreeSet::from(["todo_write".into(), "todo-write".into()]),
    };
    assert_eq!(
        policy.check(&json!("todowrite")).as_deref(),
        Some("todowrite")
    );
    assert!(policy.check(&Value::Null).is_none());
}
#[test]
fn full_json_blank_calls_become_notes_without_shifting_response_indices() {
    let mut chat = json!({"choices":[{"message":{"tool_calls":[{"type":"function","function":{"name":"","arguments":"{}"}}]},"finish_reason":"tool_calls"}]});
    snapshot(&mut chat, Protocol::Chat, &names()).unwrap();
    assert_eq!(chat["choices"][0]["finish_reason"], "stop");
    assert!(chat["choices"][0]["message"]["content"]
        .as_str()
        .unwrap()
        .contains("without a name"));
    let mut response = json!({"output":[{"type":"function_call","id":"call-one","name":"","arguments":"{}"},{"type":"function_call","id":"call-two","name":"WEBFETCH","arguments":"{}"}]});
    snapshot(&mut response, Protocol::Responses, &names()).unwrap();
    assert_eq!(response["output"][0]["type"], "message");
    assert_eq!(response["output"][1]["name"], "web_fetch");
    assert_eq!(response["output"][1]["id"], "call-two");
    assert!(snapshot(
        &mut json!({"output":[{"type":"web_search_call"}]}),
        Protocol::Responses,
        &names()
    )
    .is_err());
}
#[tokio::test]
async fn fragmented_chat_tool_names_and_arguments_are_buffered_until_a_real_finish() {
    let values = [
        json!({"id":"reply","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"real-id","type":"function","function":{"name":"ToDo","arguments":"{"}}]},"finish_reason":null}]}),
        json!({"id":"reply","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"","function":{"name":"Write","arguments":"}"}}]},"finish_reason":null}]}),
        json!({"id":"reply","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
    ];
    let frames = guard(body(&values, true), Protocol::Chat, names(), true)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<PluginResult<Vec<_>>>()
        .unwrap();
    let value = frames
        .iter()
        .filter_map(|frame| frame.value().ok())
        .find(|value| value["choices"][0]["delta"]["tool_calls"].is_array())
        .unwrap();
    let call = &value["choices"][0]["delta"]["tool_calls"][0];
    assert_eq!(call["id"], "real-id");
    assert_eq!(call["function"]["name"], "todo_write");
    assert_eq!(call["function"]["arguments"], "{}");
}
#[tokio::test]
async fn anthropic_blank_tools_become_text_and_end_turn_without_losing_usage() {
    let values = [
        json!({"type":"message_start","message":{"id":"msg","content":[],"usage":{"input_tokens":3}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call","name":"","input":{}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"secret\":1}"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":4}}),
        json!({"type":"message_stop"}),
    ];
    let frames = guard(body(&values, false), Protocol::Anthropic, names(), false)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<PluginResult<Vec<_>>>()
        .unwrap();
    assert!(!frames.iter().any(|frame| frame.text.contains("secret")));
    let last = frames
        .iter()
        .filter_map(|frame| frame.value().ok())
        .find(|value| value["type"] == "message_delta")
        .unwrap();
    assert_eq!(last["delta"]["stop_reason"], "end_turn");
    assert_eq!(last["usage"]["output_tokens"], 4);
}
#[tokio::test]
async fn responses_blank_call_arguments_are_suppressed_and_final_snapshot_is_consistent() {
    let values = [
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"call","name":""}}),
        json!({"type":"response.function_call_arguments.delta","item_id":"call","output_index":0,"delta":"private-arguments"}),
        json!({"type":"response.completed","response":{"id":"reply","status":"completed","output":[{"type":"function_call","id":"call","name":"","arguments":"private-arguments"}],"usage":{"input_tokens":2,"output_tokens":3}}}),
    ];
    let frames = guard(body(&values, false), Protocol::Responses, names(), false)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<PluginResult<Vec<_>>>()
        .unwrap();
    assert!(!frames
        .iter()
        .any(|frame| frame.text.contains("private-arguments")));
    let final_value = frames.last().unwrap().value().unwrap();
    assert_eq!(final_value["response"]["output"][0]["type"], "message");
    assert_eq!(final_value["response"]["usage"]["input_tokens"], 2);
}
#[tokio::test]
async fn cut_short_chat_is_explained_only_after_visible_text_and_without_pending_tools() {
    let visible = [
        json!({"id":"reply","choices":[{"index":0,"delta":{"content":"partial"},"finish_reason":null}]}),
    ];
    let output = guard(body(&visible, false), Protocol::Chat, names(), true)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<PluginResult<Vec<_>>>()
        .unwrap();
    assert!(output.iter().any(|frame| frame.text.contains("cut short")));
    assert_eq!(output.last().unwrap().text, "[DONE]");
    for streaming in [false, true] {
        let pending = [
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"read","arguments":"{"}}]},"finish_reason":null}]}),
        ];
        let frames = guard(body(&pending, false), Protocol::Chat, names(), streaming)
            .collect::<Vec<_>>()
            .await;
        assert!(frames.iter().any(Result::is_err));
    }
}

#[test]
fn pending_tool_bytes_are_limited_across_choices_and_calls_together() {
    let mut state = Chat::default();
    let fragment = "x".repeat(MAX_PENDING / 2 + 256);
    let first = json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"first","function":{"name":"read","arguments":fragment}}]}}]});
    assert!(state.apply(first, &names()).unwrap().is_none());
    let second = json!({"choices":[{"index":1,"delta":{"tool_calls":[{"index":0,"id":"second","function":{"name":"read","arguments":fragment}}]}}]});
    let error = state.apply(second, &names()).unwrap_err();
    assert!(error.message.contains("pending tools"));
}

#[test]
fn completed_choices_release_their_tool_budget_before_another_choice_arrives() {
    let mut state = Chat::default();
    let arguments = json!({"text":"x".repeat(MAX_PENDING / 2 + 256)}).to_string();
    for index in [0, 1] {
        let input = json!({"choices":[{"index":index,"delta":{"tool_calls":[{"index":0,"id":format!("call-{index}"),"function":{"name":"read","arguments":arguments}}]}}]});
        assert!(state.apply(input, &names()).unwrap().is_none());
        let value = state
            .apply(
                json!({"choices":[{"index":index,"delta":{},"finish_reason":"tool_calls"}]}),
                &names(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            value["choices"][0]["delta"]["tool_calls"][0]["id"],
            format!("call-{index}")
        );
        assert_eq!(state.bytes, 0);
        assert!(state.pending.is_empty());
    }
}

fn live_source(
    initial: Vec<u8>,
) -> (
    nora_ai::HttpByteStream,
    tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
) {
    let (send, receive) = tokio::sync::mpsc::unbounded_channel();
    if !initial.is_empty() {
        send.send(initial).unwrap();
    }
    let source = futures::stream::unfold(receive, |mut receive| async move {
        receive
            .recv()
            .await
            .map(|chunk: Vec<u8>| (Ok(chunk.into()), receive))
    });
    (Box::pin(source), send)
}

#[tokio::test]
async fn upstream_error_events_release_a_still_open_connection_before_the_caller_reads_again() {
    for protocol in [Protocol::Chat, Protocol::Responses, Protocol::Anthropic] {
        let event = json!({"error":{"code":"rate_limit_exceeded","message":"fixture refusal"}});
        let (source, sender) = live_source(Frame::json(event.clone(), "").encoded());
        let mut guarded = guard(source, protocol, names(), true);
        let frame = guarded.next().await.unwrap().unwrap();
        assert_eq!(frame.value().unwrap(), event);
        assert!(sender.is_closed());
        assert!(guarded.next().await.is_none());
    }
}

#[tokio::test]
async fn a_malformed_tool_snapshot_cancels_the_source_before_exposing_a_call() {
    let (source, sender) = live_source(Frame::json(json!({"type":"response.completed","response":{"output":[{"type":"function_call","id":"good","name":"read","arguments":"{}"},{"type":"web_search_call","id":"unsupported"}]}}), "response.completed").encoded());
    let mut guarded = guard(source, Protocol::Responses, names(), true);
    assert!(guarded.next().await.unwrap().is_err());
    assert!(sender.is_closed());
    assert!(guarded.next().await.is_none());
}

#[tokio::test]
async fn dropping_a_pending_guard_read_cancels_only_its_own_upstream() {
    let (source, sender) = live_source(Vec::new());
    let (other_source, other_sender) = live_source(Vec::new());
    let mut guarded = guard(source, Protocol::Chat, names(), true);
    let other = guard(other_source, Protocol::Chat, names(), true);
    let task = tokio::spawn(async move { guarded.next().await });
    tokio::task::yield_now().await;
    assert!(!sender.is_closed());
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(sender.is_closed());
    assert!(!other_sender.is_closed());
    drop(other);
    assert!(other_sender.is_closed());
}

#[tokio::test]
async fn error_frames_after_terminal_markers_are_preserved_and_cancel_remaining_input() {
    for (protocol, terminal) in [
        (Protocol::Chat, b"data: [DONE]\n\n".to_vec()),
        (
            Protocol::Responses,
            Frame::json(
                json!({"type":"response.completed","response":{"status":"completed","output":[]}}),
                "response.completed",
            )
            .encoded(),
        ),
        (
            Protocol::Anthropic,
            Frame::json(json!({"type":"message_stop"}), "message_stop").encoded(),
        ),
    ] {
        let event = json!({"error":{"code":"upstream_failure","message":"late refusal"}});
        let bytes = [terminal, Frame::json(event.clone(), "").encoded()].concat();
        let (source, sender) = live_source(bytes);
        let mut guarded = guard(source, protocol, names(), true);
        assert!(guarded.next().await.unwrap().is_ok());
        let failed = guarded.next().await.unwrap().unwrap();
        assert_eq!(failed.value().unwrap(), event);
        assert!(sender.is_closed());
        assert!(guarded.next().await.is_none());
    }
}
