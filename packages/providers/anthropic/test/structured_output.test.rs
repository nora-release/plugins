use super::*;
use serde_json::json;
#[test]
fn native_schema_is_preserved_instead_of_added_as_plain_prompt_instructions() {
    let value = json!({"type":"object","properties":{"color":{"type":"string"}},"required":["color"],"additionalProperties":false});
    assert_eq!(
        schema(&json!({"output_config":{"format":{"type":"json_schema","schema":value}}})).unwrap(),
        Some(value)
    );
    assert!(schema(&json!({})).unwrap().is_none());
    assert!(
        schema(&json!({"output_config":{"format":{"type":"json_schema","schema":"invalid"}}}))
            .is_err()
    );
}
