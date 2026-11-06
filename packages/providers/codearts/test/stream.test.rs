use super::*;
use crate::gateway::tests::collect;
use crate::provider::tests::Client;
use nora_ai::{HttpClient, HttpMethod, HttpRequest};
use std::sync::Arc;
use tokio::sync::Semaphore;

async fn response(value: Value) -> HttpStreamingResponse {
    Client::new(vec![(200, value)])
        .stream(HttpRequest {
            method: HttpMethod::Get,
            url: "https://fixture".into(),
            headers: BTreeMap::new(),
            body: None,
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn json_fallback_becomes_sse_and_the_permit_is_held_until_body_drop() {
    let gate = Arc::new(Semaphore::new(1));
    let permit = gate.clone().acquire_owned().await.unwrap();
    let raw=response(json!({"id":"actual","choices":[{"index":0,"message":{"role":"assistant","content":"你好"},"finish_reason":"stop"}]})).await;
    let head = peek(raw).await.unwrap();
    assert!(head.code.is_none());
    let response = replay(head.response, head.bytes, permit);
    assert_eq!(gate.available_permits(), 0);
    assert_eq!(response.headers["content-type"], "text/event-stream");
    let text = String::from_utf8(collect(response).await).unwrap();
    assert!(text.contains("\"delta\""));
    assert!(text.contains("你好"));
    assert!(text.contains("[DONE]"));
    assert_eq!(gate.available_permits(), 1);
}

#[tokio::test]
async fn a_late_error_keeps_already_received_text_and_never_replays_the_paid_request() {
    let raw=response(json!({"raw":"data: {\"choices\":[{\"delta\":{\"content\":\"partial answer\"}}]}\n\ndata: {\"error_code\":\"InferHub.4291.200\",\"error_msg\":\"quota\"}\n\n"})).await;
    let head = peek(raw).await.unwrap();
    assert!(head.code.is_none());
    let gate = Arc::new(Semaphore::new(1));
    let text = String::from_utf8(
        collect(replay(
            head.response,
            head.bytes,
            gate.acquire_owned().await.unwrap(),
        ))
        .await,
    )
    .unwrap();
    assert!(text.contains("partial answer"));
    assert!(text.contains("rate_limit_error"));
}

#[test]
fn folding_keeps_reasoning_tool_argument_fragments_and_actual_usage() {
    let value=completion(&[
        json!({"id":"real-id","created":100,"model":"model","choices":[{"delta":{"reasoning_content":"thinking","tool_calls":[{"index":0,"id":"tool-id","function":{"name":"read","arguments":"{\"path\":"}}]}}]}),
        json!({"choices":[{"delta":{"content":"你好","tool_calls":[{"index":0,"function":{"arguments":"\"file\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":3}})
    ],true).unwrap();
    assert_eq!(value["created"], 100);
    assert_eq!(
        value["choices"][0]["message"]["reasoning_content"],
        "thinking"
    );
    assert_eq!(
        value["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
        "{\"path\":\"file\"}"
    );
    assert_eq!(value["usage"]["prompt_tokens"], 10);
    assert!(completion(
        &[json!({"choices":[{"delta":{"content":"partial"}}]})],
        false
    )
    .is_err());
    assert!(completion(&[], true).is_err());
}

#[tokio::test]
async fn utf8_split_across_chunks_is_replayed_without_replacement_characters() {
    let raw="data: {\"choices\":[{\"delta\":{\"content\":\"你好\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".as_bytes();
    let split = raw.iter().position(|byte| *byte > 127).unwrap() + 1;
    let chunks = vec![raw[..split].to_vec(), raw[split..].to_vec()];
    let body = Box::pin(futures::stream::iter(
        chunks.into_iter().map(|bytes| Ok(bytes.into())),
    ));
    let head = peek(HttpStreamingResponse {
        status: 200,
        headers: BTreeMap::new(),
        body,
    })
    .await
    .unwrap();
    let gate = Arc::new(Semaphore::new(1));
    let text = String::from_utf8(
        collect(replay(
            head.response,
            head.bytes,
            gate.acquire_owned().await.unwrap(),
        ))
        .await,
    )
    .unwrap();
    assert!(text.contains("你好"));
    assert!(!text.contains('�'));
}
