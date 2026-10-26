use super::*;
use serde_json::{json, Value};

fn body(parameters: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"model":"k3-agent","stream":true,"messages":[{"role":"user","content":"hi"}],"tools":[{"type":"function","function":{"name":"tool","parameters":parameters}}]})).unwrap()
}

#[test]
fn all_official_schema_fixtures_and_nested_rules_match_magpies_expected_output() {
    #[derive(serde::Deserialize)]
    struct Case {
        name: String,
        input: ordered_json::Value,
        expected: Value,
    }
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("fixtures/moonshot-cases.json")).unwrap();
    for case in cases {
        let bytes = format!(
            "{{\"tools\":[{{\"function\":{{\"parameters\":{}}}}}]}}",
            serde_json::to_string(&case.input).unwrap()
        )
        .into_bytes();
        let sent: Value = serde_json::from_slice(&rewrite(bytes)).unwrap();
        assert_eq!(
            sent["tools"][0]["function"]["parameters"], case.expected,
            "case {}",
            case.name
        );
    }
}

#[test]
fn accepted_malformed_and_non_tool_bodies_remain_byte_for_byte_unchanged() {
    for bytes in [b"".to_vec(),b"not json".to_vec(),b"{\"tools\":\"x\"}".to_vec(),b"{ \"messages\": [] }".to_vec(),b"{ \"tools\": [{\"type\":\"function\",\"function\":{\"parameters\":{\"type\":\"object\",\"properties\":{\"a\":{\"type\":\"string\"}}}}}] }".to_vec()] {
        assert_eq!(rewrite(bytes.clone()),bytes);
    }
}

#[test]
fn rewrites_are_stable_and_preserve_messages_stream_and_non_schema_tool_fields() {
    let source =
        serde_json::from_str::<Value>(include_str!("fixtures/zod-derived-refs.json")).unwrap();
    let bytes = body(source);
    let first = rewrite(bytes.clone());
    assert_eq!(first, rewrite(bytes));
    assert_eq!(first, rewrite(first.clone()));
    let value: Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(value["messages"], json!([{"role":"user","content":"hi"}]));
    assert_eq!(value["stream"], true);
    assert_eq!(value["tools"][0]["function"]["name"], "tool");
}
