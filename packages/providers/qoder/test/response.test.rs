use super::*;
use crate::tests::{collect, env, Client};
async fn raw(text: String, split: bool) -> HttpStreamingResponse {
    Client::new(vec![(200, json!({"raw":text,"split":split}))])
        .stream(HttpRequest {
            method: HttpMethod::Get,
            url: "https://fixture".into(),
            headers: BTreeMap::new(),
            body: None,
        })
        .await
        .unwrap()
}
#[test]
fn queued_wrappers_are_temporary_and_never_mark_an_account_signed_out() {
    let queue = json!({"code":10605,"message":json!({"isQueued":true,"modelKey":"model","retryAfterSeconds":2.1}).to_string()});
    for value in [
        queue.clone(),
        json!({"code":403,"message":queue.to_string()}),
    ] {
        let error = failure(403, &value.to_string());
        assert_eq!(error.status, 503);
        assert_eq!(error.retry_after, Some(3));
        assert_eq!(error.sign_in.as_deref(), Some(""));
    }
    assert_eq!(
        failure(403, r#"{"code":110,"message":"spent"}"#).status,
        429
    );
    assert_eq!(
        failure(403, r#"{"code":406,"message":"bad request"}"#).status,
        400
    );
    assert_eq!(failure(500, "error in upstream response").status, 502);
}
#[tokio::test]
async fn native_tool_names_can_arrive_after_arguments_and_usage_can_follow_the_finish() {
    let text = format!(
        "{}{}{}{}",
        env(
            json!({"choices":[{"delta":{"tool_calls":[{"index":2,"id":"upstream-id","function":{"arguments":"{\"path\":"}}]}}]})
        ),
        env(
            json!({"choices":[{"delta":{"reasoning_content":"thinking","tool_calls":[{"index":2,"function":{"name":"read","arguments":"\"file\"}"}}]},"finish_reason":"tool_calls"}]})
        ),
        env(
            json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":5,"prompt_tokens_details":{"cached_tokens":20}}})
        ),
        format!(
            "data: {}\n\n",
            json!({"statusCodeValue":200,"body":"[DONE]"})
        )
    );
    let response = answer(
        raw(text, true).await,
        &json!({"model":"model","stream":false}),
    )
    .await
    .unwrap();
    let value: Value = serde_json::from_slice(&collect(response).await).unwrap();
    assert_eq!(
        value["choices"][0]["message"]["tool_calls"][0]["id"],
        "upstream-id"
    );
    assert_eq!(
        value["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
        "{\"path\":\"file\"}"
    );
    assert_eq!(value["usage"]["prompt_tokens"], 100);
    assert_eq!(value["usage"]["prompt_tokens_details"]["cached_tokens"], 20);
    assert_eq!(value["choices"][0]["finish_reason"], "tool_calls");
}
#[tokio::test]
async fn an_early_embedded_quota_error_becomes_http_429() {
    let response=answer(raw(format!("data: {}\n\n",json!({"statusCodeValue":403,"body":json!({"code":110,"message":"spent"}).to_string()})),false).await,&json!({"model":"model","stream":true})).await.unwrap();
    assert_eq!(response.status, 429);
    let value: Value = serde_json::from_slice(&collect(response).await).unwrap();
    assert!(value["error"]["message"]
        .as_str()
        .unwrap()
        .contains("spent"));
}
#[tokio::test]
async fn unicode_and_text_tools_are_streamed_without_losing_split_event_bytes() {
    let text = format!(
        "{}{}{}",
        env(
            json!({"choices":[{"delta":{"content":"你好 <tool_call>{\"name\":\"read\",\"arguments\":{\"p\":1}}</tool_call>"}}]})
        ),
        env(json!({"choices":[{"delta":{},"finish_reason":"stop"}]})),
        format!("data: {}\n\n", json!({"body":"[DONE]"}))
    );
    let response = answer(
        raw(text, true).await,
        &json!({"model":"model","stream":true}),
    )
    .await
    .unwrap();
    assert_eq!(response.status, 200);
    let text = String::from_utf8(collect(response).await).unwrap();
    assert!(text.contains("你好"));
    assert!(text.contains("tool_calls"));
    assert!(!text.contains("<tool_call>"));
    assert!(text.contains("[DONE]"));
}
#[tokio::test]
async fn truncated_native_or_text_tool_calls_never_report_a_successful_json_answer() {
    for content in [
        env(
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{"}}]},"finish_reason":"stop"}]}),
        ),
        env(
            json!({"choices":[{"delta":{"content":"<tool_call>{\"name\":\"read\",\"arguments\":"},"finish_reason":"stop"}]}),
        ),
        String::new(),
    ] {
        let response = answer(
            raw(content, false).await,
            &json!({"model":"model","stream":false}),
        )
        .await
        .unwrap();
        assert_eq!(response.status, 502);
    }
}
