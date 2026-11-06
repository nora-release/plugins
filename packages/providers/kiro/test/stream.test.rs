use super::*;
#[path = "fixture.rs"]
mod fixture;
fn reply(chunks: Vec<Vec<u8>>) -> HttpStreamingResponse {
    HttpStreamingResponse {
        status: 200,
        headers: BTreeMap::new(),
        body: Box::pin(stream::iter(
            chunks.into_iter().map(|bytes| Ok(bytes.into())),
        )),
    }
}
async fn rendered(chunks: Vec<Vec<u8>>, streaming: bool, budget: u64) -> FetchResponse {
    response(
        reply(chunks),
        "auto".into(),
        streaming,
        Names::new(),
        budget,
        200000,
    )
    .await
    .unwrap()
}
#[tokio::test]
async fn binary_frames_are_incremental_across_every_byte_and_utf8_split() {
    let bytes=[fixture::event("assistantResponseEvent",json!({"content":"中文"})),fixture::event("metadataEvent",json!({"tokenUsage":{"uncachedInputTokens":12,"outputTokens":7,"cacheReadInputTokens":2,"cacheWriteInputTokens":3}}))].concat();
    let response = rendered(
        bytes.chunks(1).map(|chunk| chunk.to_vec()).collect(),
        true,
        0,
    )
    .await;
    assert_eq!(response.status, 200);
    let text = fixture::body(response).await;
    assert!(text.contains("中文"));
    let events = nora_ai::parse_sse_events(&text);
    assert_eq!(events[0].event, "message_start");
    assert!(text.contains("\"input_tokens\":12"));
    assert!(text.contains("\"cache_read_input_tokens\":2"));
    assert!(text.ends_with("data: {\"type\":\"message_stop\"}\n\n"));
}
#[tokio::test]
async fn split_thinking_tags_signatures_and_tool_arguments_round_trip_to_anthropic() {
    let mut names = Names::new();
    names.insert("tool_wire".into(), "mcp/tool".into());
    let mut bytes = Vec::new();
    for chunk in ["<thin", "king>\nthink", "ing</thi", "nking>\nanswer"] {
        bytes.extend(fixture::event(
            "assistantResponseEvent",
            json!({"content":chunk}),
        ));
    }
    bytes.extend(fixture::event(
        "reasoningContentEvent",
        json!({"signature":"opaque-signature"}),
    ));
    bytes.extend(fixture::event(
        "toolUseEvent",
        json!({"toolUseId":"call","name":"tool_wire","input":"{\"x\":"}),
    ));
    bytes.extend(fixture::event(
        "toolUseEvent",
        json!({"toolUseId":"call","input":"1}"}),
    ));
    let response = response(
        reply(bytes.chunks(3).map(|chunk| chunk.to_vec()).collect()),
        "auto".into(),
        false,
        names,
        20000,
        0,
    )
    .await
    .unwrap();
    let body: Value = serde_json::from_str(&fixture::body(response).await).unwrap();
    assert_eq!(body["stop_reason"], "tool_use");
    let content = body["content"].as_array().unwrap();
    assert!(content.iter().any(|part| part["thinking"] == "thinking"));
    assert!(content
        .iter()
        .any(|part| part["signature"] == "opaque-signature"));
    let tool = content
        .iter()
        .find(|part| part["type"] == "tool_use")
        .unwrap();
    assert_eq!(tool["name"], "mcp/tool");
    assert_eq!(tool["input"], json!({"x":1}));
}
#[tokio::test]
async fn truncation_bad_lengths_and_crc_failures_cannot_complete_as_success() {
    let valid = fixture::event("assistantResponseEvent", json!({"content":"private-reply"}));
    let mut corrupt = valid.clone();
    corrupt[8] ^= 1;
    let mut length = valid.clone();
    length[..4].copy_from_slice(&15u32.to_be_bytes());
    for bytes in [
        valid[..valid.len() - 1].to_vec(),
        valid[..8].to_vec(),
        corrupt,
        length,
        vec![],
    ] {
        let response = rendered(vec![bytes], true, 0).await;
        assert_eq!(response.status, 502);
        let text = fixture::body(response).await;
        assert!(!text.contains("private-reply"));
    }
}
#[tokio::test]
async fn late_truncation_is_an_error_event_without_a_successful_message_stop() {
    let first = fixture::event("assistantResponseEvent", json!({"content":"hello"}));
    let response = rendered(vec![first, vec![0, 1, 2]], true, 0).await;
    let text = fixture::body(response).await;
    assert!(text.contains("event: error"));
    assert!(!text.contains("message_stop"));
}
#[tokio::test]
async fn early_quota_exception_returns_429_and_hides_sensitive_upstream_messages() {
    let bytes = fixture::headers(
        &[
            (":message-type", "exception"),
            (":exception-type", "ThrottlingException"),
        ],
        json!({"message":"private-secret usage limit"}),
    );
    let response = rendered(vec![bytes], true, 0).await;
    assert_eq!(response.status, 429);
    let text = fixture::body(response).await;
    assert!(!text.contains("private-secret"));
    assert!(text.contains("rate_limit_error"));
}
#[tokio::test]
async fn late_quota_exception_preserves_a_classifiable_error_event() {
    let bytes = [
        fixture::event("assistantResponseEvent", json!({"content":"hello"})),
        fixture::headers(
            &[
                (":message-type", "exception"),
                (":exception-type", "ThrottlingException"),
            ],
            json!({"message":"private-secret"}),
        ),
    ]
    .concat();
    let response = rendered(vec![bytes], true, 0).await;
    assert_eq!(response.status, 200);
    let text = fixture::body(response).await;
    assert!(text.contains("rate_limit_error"));
    assert!(!text.contains("private-secret"));
    assert!(!text.contains("message_stop"));
}
#[tokio::test]
async fn context_usage_fallback_and_finish_reasons_follow_the_wire_metadata() {
    let bytes = [
        fixture::event("assistantResponseEvent", json!({"content":"answer"})),
        fixture::event("contextUsageEvent", json!({"contextUsagePercentage":25.0})),
        fixture::event("messageMetadataEvent", json!({"stopReason":"MAX_TOKENS"})),
    ]
    .concat();
    let body: Value =
        serde_json::from_str(&fixture::body(rendered(vec![bytes], false, 0).await).await).unwrap();
    assert_eq!(body["usage"]["input_tokens"], 50000);
    assert_eq!(body["usage"]["output_tokens"], 2);
    assert_eq!(body["stop_reason"], "max_tokens");
}
#[tokio::test]
async fn invalid_tool_json_is_rejected_and_never_coerced_to_empty_input() {
    let bytes = fixture::event(
        "toolUseEvent",
        json!({"toolUseId":"call","name":"tool","input":"private-invalid-json"}),
    );
    let response = rendered(vec![bytes], false, 0).await;
    assert_eq!(response.status, 502);
    assert!(!fixture::body(response).await.contains("private-invalid"));
}
#[test]
fn split_thinking_tag_parser_retains_normal_text_and_incomplete_tags() {
    let mut parser = Think::default();
    assert!(parser.feed("<think").is_empty());
    assert_eq!(
        parser.feed("ing>thought</think"),
        vec![(true, "thought".into())]
    );
    assert_eq!(parser.feed("ing>text"), vec![(false, "text".into())]);
    let mut parser = Think::default();
    assert_eq!(parser.feed("normal"), vec![(false, "normal".into())]);
    let mut parser = Think::default();
    assert!(parser.feed("<think").is_empty());
    assert_eq!(parser.finish(), vec![(false, "<think".into())]);
}
#[test]
fn http_errors_are_classified_without_returning_raw_error_bodies() {
    for (body, status) in [
        ("private-secret MONTHLY_REQUEST_COUNT", 429),
        ("private-secret CONTENT_LENGTH_EXCEEDS_THRESHOLD", 400),
        ("private-secret INSUFFICIENT_MODEL_CAPACITY", 503),
    ] {
        assert_eq!(failure(403, body).status, status);
    }
}

#[tokio::test]
async fn empty_content_before_a_quota_error_still_returns_an_http_429() {
    let bytes = [
        fixture::event("assistantResponseEvent", json!({"content":""})),
        fixture::headers(
            &[
                (":message-type", "exception"),
                (":exception-type", "ThrottlingException"),
            ],
            json!({"message":"private-secret"}),
        ),
    ]
    .concat();
    let response = rendered(vec![bytes], true, 0).await;
    assert_eq!(response.status, 429);
    assert!(!fixture::body(response).await.contains("private-secret"));
}
