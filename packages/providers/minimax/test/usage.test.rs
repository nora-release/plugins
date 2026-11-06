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
fn general_and_video_buckets_keep_their_own_allowances() {
    let payload: Value = serde_json::from_str(include_str!("fixtures/usage.json")).unwrap();
    let result = parse(&payload).unwrap();
    let items = result["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["usedPercent"], 25.0);
    assert_eq!(items[0]["countsForRouting"], true);
    assert_eq!(items[0]["windowSeconds"], 18000.0);
    assert_eq!(items[2]["countsForRouting"], false);
    assert_eq!(items[2]["used"], 4.0);
    assert_eq!(items[2]["limit"], 5.0);
    assert_eq!(items[2]["unit"], "videos");
    let exhausted = parse(&json!({"base_resp":{"status_code":0},"model_remains":[{"model_name":"general","current_interval_status":2,"current_interval_remaining_percent":90,"current_weekly_status":3}]})).unwrap();
    assert_eq!(exhausted["items"].as_array().unwrap().len(), 1);
    assert_eq!(exhausted["items"][0]["usedPercent"], 100.0);
}
