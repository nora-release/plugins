use super::*;
use crate::tests::{collect, Client};
async fn response(raw: &str) -> HttpStreamingResponse {
    Client::new(vec![(200, json!({"raw":raw,"split":true}))])
        .stream(HttpRequest {
            method: HttpMethod::Get,
            url: "https://fixture".into(),
            headers: BTreeMap::new(),
            body: None,
        })
        .await
        .unwrap()
}
async fn completion(raw: &str) -> FetchResponse {
    match prepare(response(raw).await,Site::China,&json!({"model":"model","stream":false,"tools":[{"type":"function","function":{"name":"read","parameters":{"type":"object","properties":{"path":{"type":"string"}}}}}]})).await.unwrap(){Ok(response)=>response,Err(error)=>error.response()}
}
#[tokio::test]
async fn dedicated_sse_preserves_unicode_deduplicates_thoughts_and_keeps_late_usage() {
    let raw="event: output\ndata: {\"reasoning_content\":\"the complete reasoning thought\"}\n\nevent: output\ndata: {\"reasoning_content\":\"the complete reasoning thought with more\",\"response\":\"你好\"}\n\nevent: TokenUsage\ndata: {\"input_tokens\":100,\"output_tokens\":5,\"cache_read_input_tokens\":20}\n\nevent: done\ndata: {\"finish_reason\":\"max_tokens\"}\n\n";
    let value: Value = serde_json::from_slice(&collect(completion(raw).await).await).unwrap();
    assert_eq!(value["choices"][0]["message"]["content"], "你好");
    assert_eq!(
        value["choices"][0]["message"]["reasoning_content"],
        "the complete reasoning thought with more"
    );
    assert_eq!(value["usage"]["prompt_tokens"], 100);
    assert_eq!(value["usage"]["cache_read_input_tokens"], 20);
    assert_eq!(value["choices"][0]["finish_reason"], "length");
}
#[tokio::test]
async fn native_snapshot_arguments_are_completed_before_a_tool_is_given_to_the_agent() {
    let raw="event: output\ndata: {\"tool_calls\":[{\"id\":\"upstream-call\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"fi\"}}]}\n\nevent: output\ndata: {\"tool_calls\":[{\"id\":\"upstream-call\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"file\\\"}\"}}]}\n\nevent: done\ndata: {}\n\n";
    let result = completion(raw).await;
    assert_eq!(result.status, 200);
    let value: Value = serde_json::from_slice(&collect(result).await).unwrap();
    let call = &value["choices"][0]["message"]["tool_calls"][0];
    assert_eq!(call["id"], "upstream-call");
    assert_eq!(call["function"]["arguments"], "{\"path\":\"file\"}");
    assert_eq!(value["choices"][0]["finish_reason"], "tool_calls");
}
#[tokio::test]
async fn empty_response_fields_do_not_hide_an_actual_expired_account_error() {
    match prepare(response("event: message\ndata: {\"code\":1001,\"message\":\"not logged in\",\"response\":\"\"}\n\n").await,Site::China,&json!({"model":"model","stream":true})).await.unwrap(){Err(error)=>{assert_eq!(error.status,401);assert!(error.expired);},Ok(_)=>panic!("A lapsed account cannot produce a successful stream")}
}
#[tokio::test]
async fn late_errors_end_the_stream_without_a_fake_successful_finish() {
    let raw="event: output\ndata: {\"response\":\"partial text\"}\n\nevent: error\ndata: {\"code\":4008,\"message\":\"quota used\"}\n\n";
    let result = prepare(
        response(raw).await,
        Site::China,
        &json!({"model":"model","stream":true}),
    )
    .await
    .unwrap()
    .unwrap_or_else(|error| error.response());
    let text = String::from_utf8(collect(result).await).unwrap();
    assert!(text.contains("partial text"));
    assert!(text.contains("quota used"));
    assert!(!text.contains("\"finish_reason\":\"stop\""));
    assert!(text.ends_with("data: [DONE]\n\n"));
}
#[tokio::test]
async fn no_answer_or_truncated_tool_call_cannot_be_a_successful_completion() {
    for raw in ["event: done\ndata: {}\n\n","event: output\ndata: {\"response\":\"<tool_call>{\\\"name\\\":\\\"read\\\",\\\"arguments\\\":\"}\n\nevent: done\ndata: {}\n\n"]{assert_eq!(completion(raw).await.status,502);}
}
#[test]
fn token_counts_support_the_clients_aliases_and_cache_write_fields() {
    let value=tokens(&json!({"promptTokens":20,"completionTokens":4,"prompt_tokens_details":{"cached_tokens":5,"cache_write_tokens":3},"reasoning_tokens":2})).unwrap();
    assert_eq!(value["total_tokens"], 24);
    assert_eq!(value["prompt_tokens_details"]["cache_write_tokens"], 3);
    assert_eq!(value["completion_tokens_details"]["reasoning_tokens"], 2);
}

#[tokio::test]
async fn dropping_one_answer_releases_only_that_accounts_underlying_connection() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Guard(Arc<AtomicBool>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    fn connection(dropped: Arc<AtomicBool>) -> HttpStreamingResponse {
        let first = b"event: output\ndata: {\"response\":\"partial text\"}\n\n".to_vec();
        let body: nora_ai::HttpByteStream = Box::pin(futures::stream::unfold(
            (Some(first), Guard(dropped)),
            |(mut first, guard)| async move {
                if let Some(first) = first.take() {
                    return Some((Ok(first.into()), (None, guard)));
                }
                futures::future::pending().await
            },
        ));
        HttpStreamingResponse {
            status: 200,
            headers: BTreeMap::new(),
            body,
        }
    }
    let a = Arc::new(AtomicBool::new(false));
    let b = Arc::new(AtomicBool::new(false));
    let first = prepare(
        connection(a.clone()),
        Site::China,
        &json!({"model":"model","stream":true}),
    )
    .await
    .unwrap()
    .unwrap_or_else(|error| error.response());
    let second = prepare(
        connection(b.clone()),
        Site::China,
        &json!({"model":"model","stream":true}),
    )
    .await
    .unwrap()
    .unwrap_or_else(|error| error.response());
    assert!(!a.load(Ordering::SeqCst));
    assert!(!b.load(Ordering::SeqCst));
    drop(first);
    assert!(a.load(Ordering::SeqCst));
    assert!(!b.load(Ordering::SeqCst));
    drop(second);
    assert!(b.load(Ordering::SeqCst));
}
