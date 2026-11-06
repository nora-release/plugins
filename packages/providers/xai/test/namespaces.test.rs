use super::*;
use base64::Engine;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::{
    pin::Pin,
    task::{Context, Poll},
};

fn request(payload: Value) -> FetchRequest {
    serde_json::from_value(json!({"url":"https://cli-chat-proxy.grok.com/v1/responses","method":"POST","account":"fixture","credentials":{"type":"oauth","credentialProvider":"xAI","access":"fixture"},"body":base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&payload).unwrap())})).unwrap()
}
fn payload() -> Value {
    json!({"tools":[{"type":"namespace","name":"collaboration","tools":[{"type":"function","name":"spawn_agent","parameters":{"type":"object"}}]}]})
}
fn call() -> Value {
    json!({"type":"function_call","name":"collaboration__spawn_agent","id":"fc_1","call_id":"c1","arguments":"{\"text\":\"你好\"}"})
}
fn assert_restored(value: &Value) {
    assert_eq!(value["name"], "spawn_agent");
    assert_eq!(value["namespace"], "collaboration");
    assert_eq!(value["call_id"], "c1");
    assert_eq!(value["encrypted_function_args"], json!([]));
}
async fn collect(mut response: FetchResponse) -> PluginResult<Vec<u8>> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.body.next().await {
        bytes.extend(chunk?);
    }
    Ok(bytes)
}

#[test]
fn flattening_matches_official_names_and_history_only_mappings() {
    assert_eq!(
        flat_name("collaboration", "spawn_agent"),
        "collaboration__spawn_agent"
    );
    let space = "x".repeat(55);
    let name = "spawn_agent";
    let expected = Sha256::digest(format!("{space}\0{name}"));
    assert_eq!(
        flat_name(&space, name),
        format!(
            "{}_{}",
            &format!("{space}__{name}")[..55],
            expected[..4]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        )
    );
    assert_eq!(flat_name(&space, name).len(), 64);
    assert_ne!(flat_name(&space, name), flat_name(&space, "other_agent"));
    let mut item = json!({"type":"function_call","name":"spawn_agent","namespace":"collaboration","call_id":"keep"});
    assert!(flatten_call(&mut item));
    assert_eq!(item["name"], "collaboration__spawn_agent");
    assert_eq!(item["call_id"], "keep");
    assert!(item.get("namespace").is_none());
    let map = names(
        &json!({"input":[{"name":"spawn_agent","namespace":"collaboration"},{"name":"plain","namespace":"functions"}]}),
    );
    assert_eq!(map.len(), 1);
    assert!(map.contains_key("collaboration__spawn_agent"));
}

#[tokio::test]
async fn json_reply_and_nested_completion_events_restore_only_the_declared_function_calls() {
    for whole in [
        json!({"object":"response","output":[call()]}),
        json!({"type":"response.completed","response":{"object":"response","output":[call()]}}),
        json!({"type":"response.output_item.done","item":call()}),
    ] {
        let bytes = serde_json::to_vec(&whole).unwrap();
        let response = FetchResponse::bytes(
            200,
            BTreeMap::from([
                ("Content-Type".into(), "application/json".into()),
                ("Content-Length".into(), bytes.len().to_string()),
            ]),
            bytes,
        );
        let response = super::response(&request(payload()), response).unwrap();
        assert!(!response.headers.contains_key("Content-Length"));
        let parsed: Value = serde_json::from_slice(&collect(response).await.unwrap()).unwrap();
        let restored = parsed
            .get("item")
            .or_else(|| parsed.pointer("/response/output/0"))
            .or_else(|| parsed.pointer("/output/0"))
            .unwrap();
        assert_restored(restored);
    }
}

#[tokio::test]
async fn sse_split_at_every_byte_restores_added_done_completed_and_keeps_other_lines_exactly() {
    let untouched="event: response.created\r\ndata: {\"type\":\"response.created\",\"future\":1}\r\n\r\n: keep comment\n";
    let source = format!(
        "{untouched}data: {}\r\n\r\ndata: {}\n\ndata: {}\n\ndata: [DONE]",
        json!({"type":"response.output_item.added","item":call()}),
        json!({"type":"response.output_item.done","item":call()}),
        json!({"type":"response.completed","response":{"object":"response","output":[call()]}})
    );
    let response = FetchResponse {
        status: 200,
        headers: BTreeMap::from([("content-type".into(), "text/event-stream".into())]),
        body: Box::pin(stream::iter(
            source
                .as_bytes()
                .iter()
                .map(|byte| Ok(vec![*byte]))
                .collect::<Vec<_>>(),
        )),
    };
    let response = super::response(&request(payload()), response).unwrap();
    let output = String::from_utf8(collect(response).await.unwrap()).unwrap();
    assert!(output.starts_with(untouched));
    assert!(output.ends_with("data: [DONE]"));
    let values = output
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 4);
    assert_restored(&values[1]["item"]);
    assert_restored(&values[2]["item"]);
    assert_restored(&values[3]["response"]["output"][0]);
}

#[tokio::test]
async fn unrelated_unknown_invalid_sealed_and_api_key_replies_keep_original_bytes() {
    for bytes in [br#"{ "object":"response", "output":[{"type":"function_call","name":"unknown","arguments":"{}"}] }"#.to_vec(),br#"{"item":{"type":"function_call","name":"collaboration__spawn_agent","namespace":"other"}}"#.to_vec(),br#"{"future":{"type":"function_call","name":"collaboration__spawn_agent"}}"#.to_vec(),b"not JSON".to_vec()] {
        let actual=collect(super::response(&request(payload()),FetchResponse::bytes(200,BTreeMap::new(),bytes.clone())).unwrap()).await.unwrap();assert_eq!(actual,bytes);
    }
    for (status, api_key, content_type) in [
        (503, false, "application/json"),
        (200, true, "application/json"),
        (200, false, "application/octet-stream"),
    ] {
        let bytes = serde_json::to_vec(&json!({"item":call()})).unwrap();
        let mut request = request(payload());
        if api_key {
            request.credentials = Some(json!({"type":"apiKey","key":"fixture"}));
        }
        let actual = collect(
            super::response(
                &request,
                FetchResponse::bytes(
                    status,
                    BTreeMap::from([("content-type".into(), content_type.into())]),
                    bytes.clone(),
                ),
            )
            .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(actual, bytes);
    }
}

#[test]
fn buffers_are_bounded_and_an_oversized_sse_line_cannot_be_reinterpreted_from_its_tail() {
    let mut tidy = Tidy {
        names: names(&payload()),
        sse: true,
        buffer: Vec::new(),
        passthrough: false,
    };
    let huge = vec![b'x'; MAX_BUFFER + 1];
    assert_eq!(tidy.write(&huge), huge);
    assert!(tidy.buffer.is_empty());
    assert!(tidy.passthrough);
    assert_eq!(tidy.write(b"data: partial\n"), b"data: partial\n");
    assert!(!tidy.passthrough);
    let line = format!("data: {}\n", json!({"item":call()}));
    let output = tidy.write(line.as_bytes());
    assert!(String::from_utf8(output)
        .unwrap()
        .contains("\"namespace\":\"collaboration\""));
    let mut tidy = Tidy {
        names: names(&payload()),
        sse: false,
        buffer: Vec::new(),
        passthrough: false,
    };
    assert_eq!(tidy.write(&huge), huge);
    assert_eq!(tidy.write(b"rest"), b"rest");
    assert!(tidy.buffer.is_empty());
}

struct Waiting(Arc<AtomicBool>);
impl futures::Stream for Waiting {
    type Item = PluginResult<Vec<u8>>;
    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}
impl Drop for Waiting {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
#[tokio::test]
async fn cancelling_and_dropping_response_drops_upstream_without_detached_tasks() {
    let dropped = Arc::new(AtomicBool::new(false));
    let original = FetchResponse {
        status: 200,
        headers: BTreeMap::new(),
        body: Box::pin(Waiting(dropped.clone())),
    };
    let mut result = super::response(&request(payload()), original).unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(5), result.body.next())
            .await
            .is_err()
    );
    drop(result);
    assert!(dropped.load(Ordering::SeqCst));
}
#[tokio::test]
async fn buffered_partial_bytes_and_the_original_stream_error_are_both_preserved() {
    let original = FetchResponse {
        status: 200,
        headers: BTreeMap::new(),
        body: Box::pin(stream::iter([
            Ok(b"{partial".to_vec()),
            Err(PluginError::transport("fixture failure")),
        ])),
    };
    let mut result = super::response(&request(payload()), original).unwrap();
    assert_eq!(result.body.next().await.unwrap().unwrap(), b"{partial");
    assert_eq!(
        result.body.next().await.unwrap().unwrap_err().message,
        "fixture failure"
    );
    assert!(result.body.next().await.is_none());
}
