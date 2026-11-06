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
fn retired_balance_endpoint_is_reported_instead_of_showing_zero() {
    let error = parse(&json!({"code":20092,"message":"private-token","data":null})).unwrap_err();
    assert!(error.contains("retired"));
    assert!(!error.contains("private-token"));
}
