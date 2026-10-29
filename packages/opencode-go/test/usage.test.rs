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
fn an_unused_monthly_window_does_not_claim_an_invented_reset_or_duration() {
    let payload: Value = serde_json::from_str(include_str!("fixtures/usage.json")).unwrap();
    let result = parse(&payload).unwrap();
    assert!(result["items"][2].get("resetsAt").is_none());
    assert!(result["items"][2].get("windowSeconds").is_none());
    assert_eq!(result["items"][2]["usedPercent"], 0.0);
}
