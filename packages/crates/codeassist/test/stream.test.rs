use super::*;
use futures::{stream, StreamExt};
use nora_ai::{HttpClientError, HttpStreamingResponse};
fn reply(chunks: Vec<Vec<u8>>) -> HttpStreamingResponse {
    HttpStreamingResponse {
        status: 200,
        headers: BTreeMap::new(),
        body: Box::pin(stream::iter(
            chunks
                .into_iter()
                .map(|bytes| Ok::<_, HttpClientError>(bytes.into())),
        )),
    }
}
#[tokio::test]
async fn envelopes_are_unwrapped_across_arbitrary_utf8_splits_and_tool_names_are_restored() {
    let (_, names) = envelope::build(
        App::Antigravity,
        "claude-x",
        &json!({"contents":[],"tools":[{"functionDeclarations":[{"name":"mcp/tool"}]}]}),
        "p",
    )
    .unwrap();
    let wire = names.original.keys().next().unwrap();
    let bytes=format!("data: {}\n\ndata: {}",json!({"response":{"candidates":[{"content":{"parts":[{"text":"中文"},{"functionCall":{"name":wire,"args":{"x":1}},"thoughtSignature":"signature"}]}}]}}),json!({"response":{"usageMetadata":{"promptTokenCount":1}}})).into_bytes();
    let response = response(
        reply(bytes.chunks(1).map(|bytes| bytes.to_vec()).collect()),
        names,
        vec![],
        false,
    )
    .await
    .unwrap();
    let chunks = response.body.collect::<Vec<_>>().await;
    let bytes = chunks
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .concat();
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("中文"));
    assert!(text.contains("mcp/tool"));
    assert!(text.contains("signature"));
    assert!(!text.contains("\"response\""));
    assert_eq!(nora_ai::parse_sse_events(&text).len(), 2);
}
#[tokio::test]
async fn malformed_events_and_stream_errors_do_not_return_their_raw_contents() {
    let response = response(
        reply(vec![b"data: private-token\n\n".to_vec()]),
        envelope::Names::default(),
        vec!["private-token".into()],
        false,
    )
    .await
    .unwrap();
    let first = response
        .body
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .next()
        .unwrap()
        .err()
        .unwrap();
    assert!(!first.message.contains("private-token"));
}

#[tokio::test]
async fn streamed_quota_errors_keep_a_classifiable_status_without_echoing_private_messages() {
    let response = response(
        reply(vec![
            b"data: {\"error\":{\"code\":429,\"message\":\"private-token\"}}\n\n".to_vec(),
        ]),
        envelope::Names::default(),
        vec!["private-token".into()],
        false,
    )
    .await
    .unwrap();
    let chunks = response
        .body
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .concat();
    let visible = String::from_utf8(chunks).unwrap();
    assert!(visible.contains("429"));
    assert!(visible.contains("quota"));
    assert!(!visible.contains("private-token"));
}
#[tokio::test]
async fn antigravity_prompt_refusals_are_not_reported_as_account_quota_exhaustion() {
    let mut rejected = reply(vec![
        b"{\"error\":{\"message\":\"Resource has been exhausted\"}}".to_vec(),
    ]);
    rejected.status = 429;
    let rejected_response = response(rejected, envelope::Names::default(), vec![], true)
        .await
        .unwrap();
    assert_eq!(rejected_response.status, 400);
    let mut quota = reply(vec![
        b"{\"error\":{\"message\":\"You have exhausted your capacity on this model\"}}".to_vec(),
    ]);
    quota.status = 429;
    let response = response(quota, envelope::Names::default(), vec![], true)
        .await
        .unwrap();
    assert_eq!(response.status, 429);
}
