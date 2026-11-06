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
fn purchased_credits_keep_spent_included_windows_from_blocking_chat() {
    let mut payload: Value = serde_json::from_str(include_str!("fixtures/usage.json")).unwrap();
    let result = parse(&payload).unwrap();
    assert_eq!(result["items"][0]["countsForRouting"], false);
    assert_eq!(result["items"][0]["used"], 27.5);
    assert_eq!(result["balances"][0]["amount"], 25.0);
    payload["purchased"]["balance_usd"] = json!(0);
    assert_eq!(
        parse(&payload).unwrap()["items"][0]["countsForRouting"],
        true
    );
    let legacy = parse(&json!({"included":{"session":{"remaining_percent":"25"},"weekly":{"remaining_percent":50,"resets_at":"2026-11-01T00:00:00Z"}}})).unwrap();
    assert_eq!(legacy["items"][0]["usedPercent"], 75.0);
    assert!(legacy["items"][0].get("windowSeconds").is_none());
    assert_eq!(legacy["items"][1]["windowSeconds"], 604800.0);
}
