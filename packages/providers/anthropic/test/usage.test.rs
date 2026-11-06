use super::*;

#[test]
fn chat_allowances_keep_model_scopes_separate_from_other_spending() {
    let usage = parse(&json!({
        "five_hour": {"utilization": 25, "resets_at": "2026-11-01T00:00:00Z"},
        "seven_day": {"utilization": 30},
        "seven_day_opus": {"utilization": 100},
        "seven_day_sonnet": {"utilization": 40},
        "seven_day_overage_included": {"utilization": 50},
        "seven_day_oauth_apps": {"utilization": 100},
        "extra_usage": {"is_enabled": true, "used_credits": 10, "monthly_limit": 10}
    }));
    let items = usage["items"].as_array().unwrap();
    assert_eq!(items.len(), 7);
    for item in items {
        let id = item["id"].as_str().unwrap();
        if id != "extra_usage" {
            assert_eq!(
                item["windowSeconds"],
                if id == "five_hour" { 18000 } else { 604800 }
            );
        }
        assert_eq!(
            item["countsForRouting"],
            !matches!(id, "seven_day_oauth_apps" | "extra_usage")
        );
        match id {
            "seven_day_opus" => assert_eq!(item["modelIDs"], json!(["*opus*"])),
            "seven_day_sonnet" => assert_eq!(item["modelIDs"], json!(["*sonnet*"])),
            "seven_day_overage_included" => assert_eq!(item["modelIDs"], json!(["*fable*"])),
            _ => assert!(item.get("modelIDs").is_none()),
        }
    }
    assert!(timestamp(&items[0]["resetsAt"]).is_some());
}
