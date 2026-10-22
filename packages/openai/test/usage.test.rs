use super::*;

#[test]
fn only_chat_windows_count_toward_account_routing() {
    let usage = parse(&json!({
        "rate_limit": {
            "primary_window": {"used_percent": 25, "reset_at": 1793491200, "limit_window_seconds": 18000},
            "secondary_window": {"used_percent": 40}
        },
        "code_review_rate_limit": {"primary_window": {"used_percent": 100}},
        "additional_rate_limits": [
            {"metered_feature": "image_generation", "rate_limit": {"primary_window": {"used_percent": 100}}},
            {"metered_feature": "fast", "rate_limit": {"primary_window": {"used_percent": 100}}}
        ],
        "spend_control": {"individual_limit": {"limit": 10, "used": 10}}
    }));
    let items = usage["items"].as_array().unwrap();
    assert_eq!(items.len(), 6);
    for item in items {
        let id = item["id"].as_str().unwrap();
        assert_eq!(item["countsForRouting"], id.starts_with("rate_limit:"));
        assert!(item.get("modelIDs").is_none());
    }
    assert_eq!(items[0]["usedPercent"], 25.0);
    assert_eq!(items[0]["windowSeconds"], 18000.0);
    assert!(timestamp(&items[0]["resetsAt"]).is_some());
}
