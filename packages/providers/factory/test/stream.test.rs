use super::*;
use crate::fixture::{self, Reply};
fn start() -> Value {
    json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"original-id","name":"f","input":{}}})
}
fn frame(value: Value) -> String {
    format!(
        "event: {}\ndata: {value}\n\n",
        value["type"].as_str().unwrap()
    )
}
fn tools() -> Tools {
    Tools::new(BTreeSet::from(["f".into()]))
}
#[test]
fn wrapped_arguments_are_buffered_until_valid_complete_and_unwrapped_once() {
    let mut tools = tools();
    let out = tools.frame(&frame(start())).unwrap();
    assert!(String::from_utf8(out.concat())
        .unwrap()
        .contains("original-id"));
    assert!(tools.frame(&frame(json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"arguments\":{\"x\":"}}))).unwrap().is_empty());
    assert!(tools.frame(&frame(json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"18446744073709551615}}"}}))).unwrap().is_empty());
    let output = String::from_utf8(
        tools
            .frame(&frame(json!({"type":"content_block_stop","index":0})))
            .unwrap()
            .concat(),
    )
    .unwrap();
    assert!(output.contains("18446744073709551615"));
    assert!(!output.contains("arguments"));
    tools.finish().unwrap();
}
#[test]
fn partial_malformed_duplicate_or_ambiguous_wrappers_are_never_published() {
    for ending in ["data: [DONE]\n\n", "data: {\"type\":\"message_stop\"}\n\n"] {
        let mut t = tools();
        t.frame(&frame(start())).unwrap();
        assert!(t.frame(ending).is_err());
        assert!(t.finish().is_err());
    }
    let mut t = tools();
    t.frame(&frame(start())).unwrap();
    assert!(t.frame(&frame(start())).is_err());
    let mut t = tools();
    let mut start = start();
    start["content_block"]["input"] = json!({"arguments":{},"other":1});
    t.frame(&frame(start)).unwrap();
    assert!(t
        .frame(&frame(json!({"type":"content_block_stop","index":0})))
        .is_err());
}
#[tokio::test]
async fn fragmented_utf8_sse_preserves_usage_and_thought_signatures() {
    let values = vec![
        json!({"type":"message_start","message":{"usage":{"input_tokens":10}}}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"thinking","thinking":"思考","signature":"signed"}}),
        start(),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"arguments\":{\"x\":1}}"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","usage":{"output_tokens":3},"delta":{"stop_reason":"tool_use"}}),
        json!({"type":"message_stop"}),
    ];
    let response = response(Reply::sse(&values).response(), BTreeSet::from(["f".into()]))
        .await
        .unwrap();
    let text = fixture::body(response).await;
    assert!(
        text.contains("思考")
            && text.contains("signed")
            && text.contains("input_tokens")
            && text.contains("output_tokens")
    );
    assert!(text.contains("original-id"));
    assert!(!text.contains("\\\"arguments\\\""));
}
#[tokio::test]
async fn eof_with_open_wrapped_tool_is_a_stream_error() {
    let result = response(
        Reply::sse(&[start()]).response(),
        BTreeSet::from(["f".into()]),
    )
    .await
    .unwrap();
    let items = result.body.collect::<Vec<_>>().await;
    assert!(items.last().unwrap().is_err());
}
#[tokio::test]
async fn no_wrapped_tools_relay_native_body_and_remove_sensitive_headers() {
    let mut reply = Reply::json(json!({"usage":{"total_tokens":7},"choices":[]}));
    reply
        .headers
        .insert("set-cookie".into(), "private-cookie".into());
    reply.headers.insert("content-length".into(), "999".into());
    let result = response(reply.response(), BTreeSet::new()).await.unwrap();
    assert!(!result.headers.contains_key("set-cookie"));
    assert!(!result.headers.contains_key("content-length"));
    assert!(fixture::body(result).await.contains("total_tokens"));
}
