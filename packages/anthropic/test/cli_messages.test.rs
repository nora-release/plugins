use super::*;

#[test]
fn refusal_keeps_classifier_details_and_finishes_even_with_a_requested_schema() {
    for schema in [false, true] {
        for streamed in [false, true] {
            let mut receiver = Receiver::new(false).with_schema(schema);
            let details = json!({"type":"refusal","category":"cyber","explanation":"flagged"});
            let mut events = Vec::new();
            if streamed {
                events.extend(receiver.push(json!({"type":"message_start","message":{"id":"refused","content":[],"usage":{"input_tokens":3}}})).unwrap());
                events.extend(receiver.push(json!({"type":"message_delta","delta":{"stop_reason":"refusal","stop_sequence":null,"stop_details":details},"usage":{"output_tokens":2}})).unwrap());
                events.extend(receiver.push(json!({"type":"message_stop"})).unwrap());
            } else {
                let mut message = whole("refused", json!([]), "refusal");
                message["stop_sequence"] = Value::Null;
                message["stop_details"] = details.clone();
                events.extend(receiver.whole(&message).unwrap());
                events.extend(receiver.finish_whole().unwrap());
            }
            let ending = events
                .iter()
                .find(|event| event["type"] == "message_delta")
                .unwrap();
            assert_eq!(
                ending["delta"]["stop_reason"], "refusal",
                "schema={schema} streamed={streamed}: {events:?}"
            );
            assert_eq!(
                ending["delta"]["stop_details"], details,
                "schema={schema} streamed={streamed}"
            );
            assert!(events.iter().any(|event| event["type"] == "message_stop"));
            assert!(!receiver.continues);
        }
    }
}

fn whole(id: &str, content: Value, stop: &str) -> Value {
    json!({"id":id,"model":"claude-test","role":"assistant","content":content,"stop_reason":stop,"usage":{"input_tokens":2,"output_tokens":777}})
}

#[test]
fn same_id_whole_fragments_append_all_blocks_and_settle_once() {
    let mut receiver = Receiver::new(false);
    let mut output = Vec::new();
    for blocks in [
        json!([{"type":"text","text":"calling"}]),
        json!([{"type":"tool_use","id":"toolu_a","name":"mcp__nora__Read","input":{"p":1}}]),
        json!([{"type":"tool_use","id":"toolu_b","name":"mcp__nora__Read","input":{"p":2}}]),
    ] {
        let value = whole("msg_whole", blocks, "tool_use");
        assert!(!receiver.should_settle(&json!({"type":"assistant","message":value})));
        output.extend(receiver.whole(&value).unwrap());
    }
    assert!(receiver.pending_whole());
    assert_eq!(output[0]["message"]["content"], json!([]));
    assert_eq!(output[0]["message"]["stop_reason"], Value::Null);
    assert_eq!(output[0]["message"]["stop_sequence"], Value::Null);
    assert!(!output.iter().any(|event| event["type"] == "message_stop"));
    output.extend(receiver.finish_whole().unwrap());
    let starts = output
        .iter()
        .filter(|event| event["type"] == "content_block_start")
        .collect::<Vec<_>>();
    assert_eq!(
        starts
            .iter()
            .map(|event| event["index"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(starts[2]["content_block"]["input"], json!({"p":2}));
    let stop = output
        .iter()
        .find(|event| event["type"] == "message_delta")
        .unwrap();
    assert_eq!(stop["delta"]["stop_reason"], "tool_use");
    assert_eq!(
        stop["usage"]["output_tokens"], 777,
        "same-message usage must not be summed per fragment"
    );
    let mut next = Receiver::new(false);
    next.restore_history(receiver.take_history());
    assert!(next
        .whole(&whole(
            "msg_whole",
            json!([{"type":"tool_use","id":"toolu_a","name":"mcp__nora__Read","input":{"p":1}}]),
            "tool_use"
        ))
        .unwrap()
        .is_empty());
}

#[test]
fn actual_stream_ids_suppress_late_whole_copies_after_a_tool_fetch() {
    let mut previous = Receiver::new(false);
    previous
        .push(json!({"type":"message_start","message":{"id":"streamed"}}))
        .unwrap();
    previous.push(json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call","name":"mcp__nora__Read","input":{}}})).unwrap();
    previous
        .push(json!({"type":"content_block_stop","index":0}))
        .unwrap();
    previous
        .push(json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}))
        .unwrap();
    previous.push(json!({"type":"message_stop"})).unwrap();
    let mut next = Receiver::new(false);
    next.restore_history(previous.take_history());
    assert!(next
        .whole(&whole(
            "streamed",
            json!([{"type":"tool_use","id":"call","name":"mcp__nora__Read","input":{}}]),
            "tool_use"
        ))
        .unwrap()
        .is_empty());
    let output = next
        .push(json!({"type":"message_start","message":{"id":"new"}}))
        .unwrap();
    assert_eq!(output[0]["type"], "message_start");
    assert_eq!(output[0]["message"]["id"], "new");
}

#[test]
fn cut_thinking_stream_can_retry_into_same_id_whole_fragments() {
    let mut receiver = Receiver::new(false);
    let mut output = Vec::new();
    for _ in 0..2 {
        output.extend(receiver.push(json!({"type":"message_start","message":{"id":"msg_cut","usage":{"input_tokens":2,"output_tokens":5}}})).unwrap());
        output.extend(receiver.push(json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"thinking so far","signature":""}})).unwrap());
        output.extend(
            receiver
                .push(json!({"type":"content_block_stop","index":0}))
                .unwrap(),
        );
        assert!(receiver
            .push(json!({"type":"message_stop"}))
            .unwrap()
            .is_empty());
        assert!(receiver
            .whole(&whole(
                "msg_cut",
                json!([{"type":"thinking","thinking":"thinking so far"}]),
                "end_turn"
            ))
            .unwrap()
            .is_empty());
    }
    output.extend(
        receiver
            .whole(&whole(
                "msg_whole",
                json!([{"type":"thinking","thinking":"NS thinking","signature":"sig"}]),
                "end_turn",
            ))
            .unwrap(),
    );
    output.extend(
        receiver
            .whole(&whole(
                "msg_whole",
                json!([{"type":"text","text":"NONSTREAM ANSWER"}]),
                "end_turn",
            ))
            .unwrap(),
    );
    output.extend(receiver.finish_whole().unwrap());
    assert_eq!(
        output
            .iter()
            .filter(|event| event["type"] == "message_start")
            .count(),
        1
    );
    assert_eq!(
        output
            .iter()
            .filter(|event| event["type"] == "message_stop")
            .count(),
        1
    );
    let last = output
        .iter()
        .rfind(|event| event["type"] == "message_delta")
        .unwrap();
    assert_eq!(last["usage"]["output_tokens"], 787);
    assert_eq!(last["delta"]["stop_reason"], "end_turn");
    assert!(json!(output).to_string().contains("NONSTREAM ANSWER"));
}

#[test]
fn cut_after_public_words_fails_instead_of_duplicating_the_answer() {
    let mut receiver = Receiver::new(false);
    receiver
        .push(json!({"type":"message_start","message":{"id":"cut"}}))
        .unwrap();
    receiver.push(json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"shown"}})).unwrap();
    assert!(receiver.push(json!({"type":"message_stop"})).is_err());
}

#[test]
fn schema_internal_words_invalid_calls_and_retries_yield_only_final_cli_json() {
    let mut receiver = Receiver::new(false).with_schema(true);
    let mut output = Vec::new();
    for (id, stop) in [
        ("words", "end_turn"),
        ("invalid", "tool_use"),
        ("valid", "tool_use"),
    ] {
        output.extend(receiver.push(json!({"type":"message_start","message":{"id":id,"usage":{"input_tokens":10,"output_tokens":1}}})).unwrap());
        assert!(receiver.push(json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"not the schema answer"}})).unwrap().is_empty());
        assert!(receiver.push(json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"discard this too"}})).unwrap().is_empty());
        assert!(receiver
            .push(json!({"type":"content_block_stop","index":0}))
            .unwrap()
            .is_empty());
        if stop == "tool_use" {
            assert!(receiver.push(json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","name":"StructuredOutput","id":format!("schema-{id}"),"input":{}}})).unwrap().is_empty());
            assert!(receiver.push(json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"color\":3}"}})).unwrap().is_empty());
            assert!(receiver
                .push(json!({"type":"content_block_stop","index":1}))
                .unwrap()
                .is_empty());
        }
        output.extend(receiver.push(json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":5}})).unwrap());
        assert!(receiver
            .push(json!({"type":"message_stop"}))
            .unwrap()
            .is_empty());
    }
    output.extend(
        receiver
            .structured_result(&json!({"color":"blue"}))
            .unwrap(),
    );
    let text = output
        .iter()
        .filter_map(|event| event.pointer("/delta/text").and_then(Value::as_str))
        .collect::<String>();
    assert_eq!(text, "{\"color\":\"blue\"}");
    assert_eq!(
        output
            .iter()
            .filter(|event| event["type"] == "content_block_start")
            .count(),
        1
    );
    assert_eq!(output.last().unwrap()["usage"]["output_tokens"], 15);
    assert!(!json!(output).to_string().contains("StructuredOutput"));
}

#[test]
fn schema_mode_does_not_capture_caller_mcp_tools_and_missing_final_json_is_an_error() {
    let mut receiver = Receiver::new(false).with_schema(true);
    receiver
        .push(json!({"type":"message_start","message":{"id":"caller"}}))
        .unwrap();
    let output=receiver.push(json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"client","name":"mcp__nora__StructuredOutput","input":{}}})).unwrap();
    assert_eq!(output.len(), 1);
    assert_eq!(
        receiver
            .push(json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}))
            .unwrap()[0]["delta"]["stop_reason"],
        "tool_use"
    );
    assert_eq!(
        receiver.push(json!({"type":"message_stop"})).unwrap().len(),
        1
    );
    assert!(receiver.structured_result(&Value::Null).is_err());
}
