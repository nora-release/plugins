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
fn omitted_usage_matches_the_vendor_zero_usage_default() {
    let result = parse(&json!({"data":{"total_credits":20}})).unwrap();
    assert_eq!(result["balances"][0]["amount"], 20.0);
}
