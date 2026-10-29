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
fn numeric_strings_and_reset_aliases_preserve_both_quota_windows() {
    let payload: Value = serde_json::from_str(include_str!("fixtures/usage.json")).unwrap();
    let result = parse(&payload).unwrap();
    assert_eq!(result["items"][0]["usedPercent"], 12.0);
    assert_eq!(result["items"][0]["windowSeconds"], 18000.0);
    assert_eq!(result["items"][1]["windowSeconds"], 604800.0);
    assert!(result["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["countsForRouting"] == true));
}

#[test]
fn invalid_earlier_reset_aliases_do_not_hide_later_valid_timestamps() {
    for first in [json!(null), json!("not a date")] {
        let result = parse(&json!({"usage":{"limit":100,"used":5,"resetTime":first,"resetAt":"2026-11-01T00:00:00Z"}})).unwrap();
        assert_eq!(result["items"][0]["resetsAt"], "2026-11-01T00:00:00+00:00");
    }
}
