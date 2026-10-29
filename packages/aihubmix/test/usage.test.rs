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
fn key_limits_and_account_tokens_use_different_balance_units() {
    assert_eq!(
        parse(&json!({"total_usage":12.5})).unwrap()["balances"][0]["amount"],
        12.5
    );
    assert_eq!(
        parse(&json!({"success":true,"data":{"quota":2500000}})).unwrap()["balances"][0]["amount"],
        5.0
    );
    assert!(parse(&json!({"total_usage":-1}))
        .unwrap_err()
        .contains("no spending limit"));
    assert!(!parse(&json!({"success":false,"message":"private-token"}))
        .unwrap_err()
        .contains("private-token"));
}
