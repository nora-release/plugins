use super::*;
use serde_json::json;

#[test]
fn real_response_fields_are_normalized_without_inventing_unknown_totals() {
    let payload: Value = serde_json::from_str(include_str!("fixtures/usage.json")).unwrap();
    let result = parse(&payload).unwrap();
    assert_eq!(result["available"], true);
    assert!(parse(&json!({})).is_err());
    assert!(!result.to_string().contains("unexposed-private-field"));
}

#[test]
fn token_windows_count_for_chat_while_mcp_allowances_do_not() {
    let payload: Value = serde_json::from_str(include_str!("fixtures/usage.json")).unwrap();
    let result = parse(&payload).unwrap();
    assert_eq!(result["items"][0]["windowSeconds"], 18000.0);
    assert_eq!(result["items"][1]["windowSeconds"], 604800.0);
    assert_eq!(result["items"][0]["countsForRouting"], true);
    assert_eq!(result["items"][2]["countsForRouting"], false);
    assert_eq!(result["account"]["plan"], "pro");
    assert!(
        parse(&json!({"success":false,"message":"private-token","data":{"limits":[]}}))
            .unwrap_err()
            .contains("refused")
    );
}
