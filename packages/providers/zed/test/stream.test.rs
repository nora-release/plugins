use super::*;
use crate::fixture::{self, Reply};
#[tokio::test]
async fn arbitrary_network_chunking_does_not_change_native_anthropic_events() {
    let response = response(
        Wire::Anthropic,
        true,
        Reply::lines(&fixture::anthropic(), true).response(),
    )
    .await
    .unwrap();
    assert_eq!(response.status, 200);
    let body = fixture::body(response).await;
    assert!(body.contains("event: content_block_start\n"));
    assert!(body.contains("original-tool-id"));
    assert!(body.contains("original_name"));
    assert!(body.contains("\"output_tokens\":2"));
    assert!(!body.contains("\"status\":\"stream_ended\""));
}
#[tokio::test]
async fn status_failure_before_any_answer_preserves_http_status_and_hides_private_body() {
    let rows = [
        json!({"status":"started"}),
        json!({"event":{"type":"message_start","message":{"id":"m"}}}),
        json!({"status":{"failed":{"code":"upstream_http_429","message":"private-zed-access private prompt"}}}),
    ];
    let response = response(Wire::Anthropic, true, Reply::lines(&rows, true).response())
        .await
        .unwrap();
    assert_eq!(response.status, 429);
    let body = fixture::body(response).await;
    assert!(!body.contains("private"));
    assert!(body.contains("rate_limit_error"));
}
#[tokio::test]
async fn status_failure_after_output_is_a_terminal_error_not_a_replayed_request() {
    let rows = [
        json!({"event":{"choices":[{"index":0,"delta":{"content":"hello"}}]}}),
        json!({"status":{"failed":{"code":"payment_required","message":"private data"}}}),
    ];
    let response = response(Wire::Chat, true, Reply::lines(&rows, true).response())
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    let body = fixture::body(response).await;
    assert!(body.contains("hello"));
    assert!(body.contains("\"code\":402"));
    assert!(!body.contains("private data"));
    assert!(!body.contains("[DONE]"));
}
#[tokio::test]
async fn text_only_chat_cut_short_keeps_text_with_an_explicit_notice_but_partial_tools_fail() {
    let text = [
        json!({"event":{"id":"c","model":"grok-model","choices":[{"index":0,"delta":{"content":"part"}}]}}),
    ];
    let value = response(Wire::Chat, true, Reply::lines(&text, true).response())
        .await
        .unwrap();
    let body = fixture::body(value).await;
    assert!(body.contains("part"));
    assert!(body.contains("reply was cut short"));
    assert!(body.contains("[DONE]"));
    let tool = [
        json!({"event":{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"f","arguments":"{"}}]}}]}}),
    ];
    let value = response(Wire::Chat, true, Reply::lines(&tool, true).response())
        .await
        .unwrap();
    let body = fixture::body(value).await;
    assert!(body.contains("\"code\":502"));
    assert!(!body.contains("[DONE]"));
}
#[tokio::test]
async fn successful_whole_reply_requires_the_wrapped_stream_end_marker() {
    let mut rows = fixture::anthropic();
    rows.pop();
    let value = response(Wire::Anthropic, false, Reply::lines(&rows, true).response())
        .await
        .unwrap();
    assert_eq!(value.status, 502);
    let value = response(
        Wire::Anthropic,
        false,
        Reply::lines(&fixture::anthropic(), true).response(),
    )
    .await
    .unwrap();
    let body: Value = serde_json::from_str(&fixture::body(value).await).unwrap();
    assert_eq!(body["content"][0]["input"], json!({"x":1}));
}
#[tokio::test]
async fn malformed_utf8_and_truncated_json_are_explicit_failures() {
    for bytes in [vec![255, b'\n'], b"{\"event\":".to_vec()] {
        let reply = Reply {
            status: 200,
            headers: BTreeMap::new(),
            chunks: vec![bytes],
        };
        assert!(response(Wire::Chat, true, reply.response()).await.is_err());
    }
}
#[tokio::test]
async fn cancellation_drops_only_the_owned_network_body() {
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
    tx.send(b"{\"event\":{\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}}\n".to_vec())
        .await
        .unwrap();
    let native = HttpStreamingResponse {
        status: 200,
        headers: BTreeMap::from([(
            "x-zed-server-supports-status-messages".into(),
            "true".into(),
        )]),
        body: Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|b| (Ok(b.into()), rx))
        })),
    };
    let reply = response(Wire::Chat, true, native).await.unwrap();
    assert!(!tx.is_closed());
    drop(reply);
    assert!(tx.is_closed());
}
#[test]
fn upstream_status_mapping_does_not_confuse_billing_and_authentication() {
    assert_eq!(failed_status("upstream_http_401"), 401);
    assert_eq!(failed_status("payment_required"), 402);
    assert_eq!(failed_status("rate_limit_error"), 429);
    assert_eq!(failed_status("overloaded"), 529);
    assert_eq!(failed_status("http_999"), 502);
}
