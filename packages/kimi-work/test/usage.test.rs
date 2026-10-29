use super::*;

#[test]
fn only_the_shared_membership_allowance_counts_for_work_routing() {
    let usage = parse(&json!({
        "subscriptionBalance":{"amountUsedRatio":0.42,"expireTime":"2030-01-01T00:00:00Z"},
        "ratelimitCode5h":{"ratio":1}, "ratelimitCode7d":{"ratio":1},
        "email":"private-profile@example.invalid"
    }));
    let items = usage["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["usedPercent"], 42.0);
    assert_eq!(items[0]["countsForRouting"], true);
    assert!(items[0].get("resetsAt").is_none());
    assert!(!usage.to_string().contains("private-profile"));
}

#[test]
fn an_absent_or_invalid_membership_allowance_stays_unknown() {
    for payload in [
        json!({}),
        json!({"subscriptionBalance":{"amountUsedRatio":"0"}}),
        json!({"ratelimitCode5h":{"ratio":1}}),
    ] {
        let usage = parse(&payload);
        assert_eq!(usage["available"], false);
        assert!(usage["items"].as_array().unwrap().is_empty());
    }
}
