use super::*;
use serde_json::json;

#[test]
fn codex_final_fetch_is_zstd_after_fast_mode_normalization_and_decodes_to_the_final_body() {
    let target = Url::parse("https://chatgpt.com/backend-api/codex/responses").unwrap();
    let mut headers = BTreeMap::from([("content-length".into(), "1".into())]);
    let mut body = Some(
        serde_json::to_vec(&json!({"model":"gpt-6-luna","input":"hello","service_tier":"fast"}))
            .unwrap(),
    );
    OpenAI
        .finalize_fetch(
            &json!({"type":"oauth","access":"synthetic"}),
            &target,
            &mut headers,
            &mut body,
        )
        .unwrap();
    assert_eq!(
        headers.get("content-encoding").map(String::as_str),
        Some("zstd")
    );
    assert!(!headers.contains_key("content-length"));
    let decoded = zstd::stream::decode_all(body.as_deref().unwrap()).unwrap();
    let value: Value = serde_json::from_slice(&decoded).unwrap();
    assert_eq!(value["input"][0]["content"][0]["text"], "hello");
    assert!(value.get("service_tier").is_none());
}
#[test]
fn codex_compact_api_key_and_other_hosts_are_not_compressed() {
    for (url, oauth) in [
        (
            "https://chatgpt.com/backend-api/codex/responses/compact",
            true,
        ),
        ("https://api.openai.com/v1/responses", false),
        ("https://relay.example/backend-api/codex/responses", true),
    ] {
        let mut headers = BTreeMap::new();
        let original = b"{\"model\":\"chat\",\"input\":\"hi\"}".to_vec();
        let mut body = Some(original.clone());
        OpenAI
            .finalize_fetch(
                &json!({"type":if oauth {"oauth"}else{"apiKey"}}),
                &Url::parse(url).unwrap(),
                &mut headers,
                &mut body,
            )
            .unwrap();
        assert_eq!(body.unwrap(), original);
        assert!(!headers.contains_key("content-encoding"));
    }
}
#[test]
fn codex_compression_respects_existing_encoding_and_never_encodes_twice() {
    let target = Url::parse("https://chatgpt.com/backend-api/codex/responses").unwrap();
    for encoding in ["zstd", "gzip"] {
        let mut headers = BTreeMap::from([("Content-Encoding".into(), encoding.into())]);
        let mut body = Some(vec![1, 2, 3]);
        OpenAI
            .finalize_fetch(&json!({"type":"oauth"}), &target, &mut headers, &mut body)
            .unwrap();
        assert_eq!(body, Some(vec![1, 2, 3]));
        assert_eq!(headers.len(), 1);
    }
}
