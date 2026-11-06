use super::*;

#[test]
fn native_search_markers_and_options_are_owned_by_the_cli_as_in_magpie() {
    assert!(!requested(&json!({"tools":[]})).unwrap());
    for kind in [
        "web_search_20250305",
        "web_search_20260209",
        "web_search_20260318",
        "web_search_future",
    ] {
        for options in [
            json!({}),
            json!({"max_uses":1,"allowed_domains":["example.com"],"user_location":{"type":"approximate","country":"US"},"cache_control":{"type":"ephemeral"},"allowed_callers":["code_execution_20260120"],"response_inclusion":"excluded","strict":false,"defer_loading":false}),
            json!({"max_uses":null,"unknown_option":"ignored"}),
        ] {
            let mut definition = options;
            definition["type"] = json!(kind);
            definition["name"] = json!("web_search");
            assert!(requested(&json!({"tools":[definition]})).unwrap());
        }
    }
    assert!(!is_search(
        &json!({"type":"web_search_20250305","name":"caller","input_schema":{"type":"object"}})
    ));
}

#[test]
fn own_search_turns_become_one_reply_with_complete_usage_and_contiguous_blocks() {
    let events = [
        json!({"type":"message_start","message":{"id":"m1","usage":{"input_tokens":10,"cache_read_input_tokens":100}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Let me look. "}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"search","name":"WebSearch","input":{}}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"query\":\"go\"}"}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}),
        json!({"type":"message_stop"}),
        json!({"type":"message_start","message":{"id":"m2","usage":{"input_tokens":30,"cache_read_input_tokens":50}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Go 1.27.1"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}),
        json!({"type":"message_stop"}),
    ];
    let mut stream = SearchStream::new(true);
    let output = events
        .into_iter()
        .flat_map(|event| stream.push(event).unwrap())
        .collect::<Vec<_>>();
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
    assert!(!serde_json::to_string(&output)
        .unwrap()
        .contains("WebSearch"));
    assert!(stream.seen("m1") && stream.seen("m2"));
    let blocks = output
        .iter()
        .filter(|event| event["type"] == "content_block_start")
        .map(|event| event["index"].as_u64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(blocks, vec![0, 1]);
    let last = output
        .iter()
        .rfind(|event| event["type"] == "message_delta")
        .unwrap();
    assert_eq!(last["delta"]["stop_reason"], "end_turn");
    assert_eq!(
        last["usage"],
        json!({"input_tokens":40,"output_tokens":12,"cache_read_input_tokens":150})
    );
}

#[test]
fn caller_mcp_calls_still_end_the_visible_tool_turn() {
    let mut stream = SearchStream::new(true);
    for event in [
        json!({"type":"message_start","message":{"id":"m"}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","name":"mcp__nora__read","id":"read"}}),
    ] {
        stream.push(event).unwrap();
    }
    let event = json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":2}});
    assert_eq!(
        stream.push(event).unwrap()[0]["delta"]["stop_reason"],
        "tool_use"
    );
    assert_eq!(
        stream.push(json!({"type":"message_stop"})).unwrap().len(),
        1
    );
}

#[test]
fn search_stream_bounds_internal_block_indexes() {
    let mut stream = SearchStream::new(true);
    assert!(stream.push(json!({"type":"content_block_start","index":10000,"content_block":{"type":"tool_use","name":"WebSearch"}})).is_err());
}

fn search_result(id: &str) -> Value {
    let mut value: Value =
        serde_json::from_str(include_str!("testdata/cli-web-search-result.json")).unwrap();
    value["message"]["content"][0]["tool_use_id"] = json!(id);
    value
}

fn own_call(stream: &mut SearchStream, id: &str, index: usize) {
    assert!(stream.push(json!({"type":"content_block_start","index":index,"content_block":{"type":"tool_use","id":id,"name":"WebSearch","input":{}}})).unwrap().is_empty());
    assert!(stream
        .push(json!({"type":"content_block_stop","index":index}))
        .unwrap()
        .is_empty());
}

#[test]
fn cli_result_becomes_paired_hosted_blocks_once_with_contiguous_indexes() {
    let mut stream = SearchStream::new(true);
    stream
        .push(json!({"type":"message_start","message":{"id":"one"}}))
        .unwrap();
    let before = stream.push(json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"Checking."}})).unwrap();
    own_call(&mut stream, "search-own", 1);
    let mut output = before;
    output.extend(stream.result(&search_result("search-own")).unwrap());
    assert!(stream
        .result(&search_result("search-own"))
        .unwrap()
        .is_empty());
    stream
        .push(json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}))
        .unwrap();
    assert!(stream
        .push(json!({"type":"message_stop"}))
        .unwrap()
        .is_empty());
    output.extend(
        stream
            .push(json!({"type":"message_start","message":{"id":"two"}}))
            .unwrap(),
    );
    output.extend(stream.push(json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"Done."}})).unwrap());
    let blocks = output
        .iter()
        .filter(|event| event["type"] == "content_block_start")
        .collect::<Vec<_>>();
    assert_eq!(
        blocks
            .iter()
            .map(|event| event["index"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    assert_eq!(blocks[1]["content_block"]["type"], "server_tool_use");
    let id = blocks[1]["content_block"]["id"].as_str().unwrap();
    assert!(id.starts_with("srvtoolu_"));
    assert_eq!(blocks[2]["content_block"]["tool_use_id"], id);
    assert_eq!(
        blocks[2]["content_block"]["content"][0]["url"],
        "https://example.com/docs"
    );
    assert_eq!(
        blocks[2]["content_block"]["content"][0]["encrypted_content"],
        ""
    );
    let args = output
        .iter()
        .find_map(|event| event.pointer("/delta/partial_json").and_then(Value::as_str))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(args).unwrap(),
        json!({"query":"official documentation"})
    );
    assert!(!json!(output).to_string().contains("WebSearch"));
}

#[test]
fn late_results_and_replayed_calls_keep_stable_ownership_across_fetches() {
    let mut prior = SearchStream::new(true);
    prior
        .push(json!({"type":"message_start","message":{"id":"prior"}}))
        .unwrap();
    own_call(&mut prior, "search-own", 0);
    let mut resumed = SearchStream::new(true);
    resumed.restore_history(prior.take_history());
    assert!(resumed
        .result(&search_result("search-own"))
        .unwrap()
        .is_empty());
    let output = resumed
        .push(json!({"type":"message_start","message":{"id":"next"}}))
        .unwrap();
    assert_eq!(output[0]["type"], "message_start");
    let first_id = output[1]["content_block"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(output[1]["index"], 0);
    assert_eq!(output[4]["index"], 1);
    assert!(resumed
        .result(&search_result("search-own"))
        .unwrap()
        .is_empty());
    own_call(&mut resumed, "search-two", 0);
    let next = resumed.result(&search_result("search-two")).unwrap();
    assert_eq!(next[0]["index"], 2);
    assert_ne!(next[0]["content_block"]["id"], first_id);
    let mut third = SearchStream::new(true);
    third.restore_history(resumed.take_history());
    assert!(third
        .result(&search_result("search-own"))
        .unwrap()
        .is_empty());
    assert!(third
        .result(&search_result("search-two"))
        .unwrap()
        .is_empty());
}

#[test]
fn mixed_mcp_and_owned_search_results_keep_the_caller_tool_boundary() {
    let mut stream = SearchStream::new(true);
    stream
        .push(json!({"type":"message_start","message":{"id":"mixed"}}))
        .unwrap();
    own_call(&mut stream, "search-own", 0);
    let mcp = json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"mcp-call","name":"mcp__nora__lookup","input":{}}});
    let visible = stream.push(mcp).unwrap();
    assert_eq!(visible[0]["content_block"]["name"], "mcp__nora__lookup");
    let mut result = search_result("mcp-call");
    assert!(stream.result(&result).unwrap().is_empty());
    result["message"]["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"tool_result","tool_use_id":"search-own","content":"search"}));
    assert_eq!(stream.result(&result).unwrap().len(), 5);
    let delta = stream
        .push(json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}))
        .unwrap();
    assert_eq!(delta[0]["delta"]["stop_reason"], "tool_use");
    assert_eq!(
        stream.push(json!({"type":"message_stop"})).unwrap().len(),
        1
    );
}
