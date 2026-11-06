use super::*;
use crate::{
    fixture::{self, Reply},
    protobuf::Pb,
};
fn chat(stream: bool) -> Value {
    json!({"model":"swe-2","stream":stream})
}
#[tokio::test]
async fn fragmented_connect_reply_preserves_thought_text_tools_and_usage() {
    let frames = vec![
        Pb::new().text(9, "思考").text(3, "answer"),
        Pb::new().bytes(
            6,
            Pb::new()
                .text(1, "Bash:0#native")
                .text(2, "bash")
                .text(3, "{\"command\":"),
        ),
        Pb::new().bytes(6, Pb::new().text(1, "Bash:0#native").text(3, "\"ls\"}")),
        Pb::new()
            .int(5, 10)
            .bytes(7, Pb::new().int(2, 10).int(3, 5).int(4, 2).int(5, 3)),
    ];
    for streaming in [false, true] {
        let result = response(
            Reply::stream(&frames, Some(json!({}))).response(),
            &chat(streaming),
        )
        .await
        .unwrap();
        assert_eq!(result.status, 200);
        let body = fixture::body(result).await;
        assert!(body.contains("思考"));
        assert!(body.contains("answer"));
        assert!(body.contains(&request::out_id("Bash:0#native")));
        assert!(body.contains("cached_tokens"));
        if !streaming {
            let value: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(value["usage"]["prompt_tokens"], 15);
            assert_eq!(value["usage"]["completion_tokens"], 5);
            assert_eq!(
                value["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
                "{\"command\":\"ls\"}"
            );
        }
    }
}
#[tokio::test]
async fn parallel_native_ids_cannot_steal_each_others_argument_pieces() {
    let frames = vec![
        Pb::new().bytes(6, Pb::new().text(1, "a").text(2, "read").text(3, "{\"x\":")),
        Pb::new().bytes(
            6,
            Pb::new().text(1, "b").text(2, "write").text(3, "{\"y\":"),
        ),
        Pb::new().bytes(6, Pb::new().text(1, "a").text(3, "1}")),
        Pb::new().bytes(6, Pb::new().text(1, "b").text(3, "2}")),
    ];
    let result = response(
        Reply::stream(&frames, Some(json!({}))).response(),
        &chat(false),
    )
    .await
    .unwrap();
    let value: Value = serde_json::from_str(&fixture::body(result).await).unwrap();
    let tools = value["choices"][0]["message"]["tool_calls"]
        .as_array()
        .unwrap();
    assert_eq!(tools[0]["id"], "a");
    assert_eq!(tools[0]["function"]["arguments"], "{\"x\":1}");
    assert_eq!(tools[1]["id"], "b");
    assert_eq!(tools[1]["function"]["arguments"], "{\"y\":2}");
}
#[tokio::test]
async fn early_connect_errors_keep_real_status_without_reflecting_secrets() {
    for (code, status) in [
        ("unauthenticated", 401),
        ("resource_exhausted", 429),
        ("invalid_argument", 400),
        ("unavailable", 503),
    ] {
        let result = response(
            Reply::stream(
                &[],
                Some(json!({"error":{"code":code,"message":"private-token prompt"}})),
            )
            .response(),
            &chat(true),
        )
        .await
        .unwrap();
        assert_eq!(result.status, status);
        assert!(!fixture::body(result).await.contains("private"));
    }
}
#[tokio::test]
async fn eof_after_complete_frames_without_connect_end_is_not_success() {
    let result = response(
        Reply::stream(&[Pb::new().text(3, "partial answer")], None).response(),
        &chat(false),
    )
    .await
    .unwrap();
    assert_eq!(result.status, 502);
    let result = response(
        Reply::stream(&[Pb::new().text(3, "partial answer")], None).response(),
        &chat(true),
    )
    .await
    .unwrap();
    let values = result.body.collect::<Vec<_>>().await;
    assert!(values.last().unwrap().is_err());
}
#[tokio::test]
async fn incomplete_or_unowned_tool_arguments_never_finish_successfully() {
    for frame in [
        Pb::new().bytes(
            6,
            Pb::new()
                .text(1, "a")
                .text(2, "bash")
                .text(3, "{\"command\":\"cut"),
        ),
        Pb::new().bytes(6, Pb::new().text(3, "unowned")),
    ] {
        let result = response(
            Reply::stream(&[frame], Some(json!({}))).response(),
            &chat(false),
        )
        .await
        .unwrap();
        assert_eq!(result.status, 502);
    }
}
#[tokio::test]
async fn truncated_headers_payloads_bad_flags_and_gzip_are_errors() {
    for bytes in [
        vec![0],
        vec![0, 0, 0, 0, 5, 1],
        vec![4, 0, 0, 0, 0],
        vec![1, 0, 0, 0, 1, 0],
    ] {
        let result = response(Reply::bytes(bytes).response(), &chat(false))
            .await
            .unwrap();
        assert_eq!(result.status, 502);
    }
}
#[tokio::test]
async fn compressed_native_response_frames_decode_incrementally() {
    let text = "answer".repeat(2000);
    let result = response(
        Reply::stream(&[Pb::new().text(3, &text).int(5, 2)], Some(json!({}))).response(),
        &chat(false),
    )
    .await
    .unwrap();
    assert_eq!(result.status, 200);
    let value: Value = serde_json::from_str(&fixture::body(result).await).unwrap();
    assert_eq!(value["choices"][0]["message"]["content"], text);
}
#[tokio::test]
async fn failure_after_visible_output_is_terminal_and_never_replayed() {
    let result = response(
        Reply::stream(
            &[Pb::new().text(3, "visible")],
            Some(json!({"error":{"code":"resource_exhausted","message":"private"}})),
        )
        .response(),
        &chat(true),
    )
    .await
    .unwrap();
    let values = result.body.collect::<Vec<_>>().await;
    let text = values
        .iter()
        .filter_map(|v| v.as_ref().ok())
        .flatten()
        .copied()
        .collect::<Vec<_>>();
    let text = String::from_utf8(text).unwrap();
    assert!(text.contains("visible"));
    assert!(!text.contains("finish_reason\":\"stop"));
    assert!(values.last().unwrap().is_err());
}
struct Dropped {
    first: Option<Vec<u8>>,
    drops: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl futures::Stream for Dropped {
    type Item = Result<bytes::Bytes, nora_ai::HttpClientError>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        if let Some(first) = self.first.take() {
            std::task::Poll::Ready(Some(Ok(first.into())))
        } else {
            std::task::Poll::Pending
        }
    }
}
impl Drop for Dropped {
    fn drop(&mut self) {
        self.drops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}
#[tokio::test]
async fn cancelling_one_response_drops_only_its_own_native_stream() {
    let one = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let two = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let make = |drops: std::sync::Arc<std::sync::atomic::AtomicUsize>| HttpStreamingResponse {
        status: 200,
        headers: BTreeMap::new(),
        body: Box::pin(Dropped {
            first: Some(protobuf::frame(&Pb::new().text(3, "visible").0).unwrap()),
            drops,
        }),
    };
    let first = response(make(one.clone()), &chat(true)).await.unwrap();
    let other = response(make(two.clone()), &chat(true)).await.unwrap();
    drop(first);
    assert_eq!(one.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(two.load(std::sync::atomic::Ordering::SeqCst), 0);
    drop(other);
    assert_eq!(two.load(std::sync::atomic::Ordering::SeqCst), 1);
}
#[tokio::test]
async fn omitted_arguments_are_empty_only_for_an_advertised_optional_parameter_tool() {
    let frames = [Pb::new().bytes(6, Pb::new().text(1, "a").text(2, "ping"))];
    let mut request = chat(false);
    request["tools"] = json!([{"type":"function","function":{"name":"ping","parameters":{"type":"object","properties":{}}}}]);
    let result = response(Reply::stream(&frames, Some(json!({}))).response(), &request)
        .await
        .unwrap();
    assert_eq!(result.status, 200);
    let value: Value = serde_json::from_str(&fixture::body(result).await).unwrap();
    assert_eq!(
        value["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
        "{}"
    );
    request["tools"][0]["function"]["parameters"]["required"] = json!(["command"]);
    assert_eq!(
        response(Reply::stream(&frames, Some(json!({}))).response(), &request)
            .await
            .unwrap()
            .status,
        502
    );
}
