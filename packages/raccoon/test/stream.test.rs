use super::*;
use nora_provider_plugin_sdk::PluginResult;
#[test]
fn rewriting_changes_only_reported_model_and_preserves_native_tool_payload() {
    let bytes=b"data: {\"model\":\"actual-upstream\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call\",\"function\":{\"name\":\"read\",\"arguments\":\"{}\"}}]}}]}\r\n";
    let value = rewrite_line(bytes, "asked-model");
    assert!(value.ends_with(b"\r\n"));
    let parsed: Value = serde_json::from_slice(&value[6..value.len() - 2]).unwrap();
    assert_eq!(parsed["model"], "asked-model");
    assert_eq!(
        parsed["choices"][0]["delta"]["tool_calls"][0]["function"]["name"],
        "read"
    );
    assert_eq!(rewrite_line(b"data: [DONE]\n", "asked"), b"data: [DONE]\n");
}
#[tokio::test]
async fn utf8_and_sse_lines_split_across_chunks_are_reassembled_before_rewriting() {
    let raw = "data: {\"model\":\"old\",\"content\":\"中文\"}\n\ndata: [DONE]\n\n"
        .as_bytes()
        .to_vec();
    let chunks: Vec<_> = raw
        .chunks(3)
        .map(|bytes| Ok(bytes.to_vec().into()))
        .collect();
    let body = Box::pin(futures::stream::iter(chunks));
    let output = rewrite(body, "requested".into())
        .collect::<Vec<PluginResult<Vec<u8>>>>()
        .await;
    let bytes: Vec<u8> = output.into_iter().flat_map(Result::unwrap).collect();
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("中文"));
    assert!(text.contains("requested"));
    assert!(!text.contains("\"old\""));
    assert!(text.ends_with("data: [DONE]\n\n"));
}
