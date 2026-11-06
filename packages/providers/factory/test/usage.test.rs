use super::*;
use crate::fixture;
fn limits() -> Value {
    json!({"limits":{"standard":{"fiveHour":{"usedPercent":101,"windowEnd":"2030-01-01T00:00:00Z"},"weekly":{"usedPercent":33,"windowEnd":1893456000000u64}},"core":{"monthly":{"usedPercent":-2,"windowEnd":"1893456000000"}}},"extraUsageBalanceCents":1250})
}
#[test]
fn actual_windows_keep_model_scopes_duration_and_reset() {
    let account = Account::parse(&fixture::saved()).unwrap();
    let value = parse(&limits(), &account, "one", false).unwrap();
    let items = value["items"].as_array().unwrap();
    assert_eq!(items.len(), 4);
    assert_eq!(items[0]["usedPercent"], 100.0);
    assert_eq!(items[0]["windowSeconds"], 18000);
    assert_eq!(items[0]["countsForRouting"], true);
    assert_eq!(items[1]["windowSeconds"], 604800);
    assert_eq!(items[2]["windowSeconds"], 2592000);
    assert_eq!(items[2]["usedPercent"], 0.0);
    assert!(items[0].get("modelIDs").is_none());
    assert_eq!(items[0]["notModels"], json!(models::ids(true)));
    assert_eq!(items[2]["modelIDs"], json!(models::ids(true)));
    assert_eq!(items[3]["balance"], 12.5);
    assert_eq!(items[3]["countsForRouting"], false);
    assert_eq!(items[0]["resetsAt"], items[2]["resetsAt"]);
}
#[test]
fn spendable_extra_balance_prevents_false_exhaustion() {
    let mut value = limits();
    value["extraUsageAllowed"] = json!(true);
    let value = parse(
        &value,
        &Account::parse(&fixture::saved()).unwrap(),
        "one",
        false,
    )
    .unwrap();
    for item in value["items"].as_array().unwrap() {
        assert_eq!(item["countsForRouting"], false);
    }
}
#[test]
fn bad_or_missing_percentages_do_not_become_zero_usage() {
    let account = Account::parse(&fixture::saved()).unwrap();
    for value in [
        json!({}),
        json!({"limits":{"standard":{}}}),
        json!({"limits":{"standard":{"fiveHour":{}}}}),
        json!({"limits":{"standard":{"weekly":{"usedPercent":"private"}}}}),
    ] {
        assert!(parse(&value, &account, "one", false).is_err());
    }
}
#[test]
fn window_numeric_timestamps_are_milliseconds() {
    assert_eq!(
        end(&json!(1893456000000u64)),
        end(&json!("2030-01-01T00:00:00Z"))
    );
    assert!(end(&json!("tomorrow")).is_none());
}

#[test]
fn standard_only_counts_every_model_and_ignores_core_and_extra_allowance() {
    let mut value = limits();
    value["extraUsageAllowed"] = json!(true);
    let parsed = parse(
        &value,
        &Account::parse(&fixture::saved()).unwrap(),
        "one",
        true,
    )
    .unwrap();
    let items = parsed["items"].as_array().unwrap();
    for item in items {
        if item["id"].as_str().unwrap().starts_with("standard-") {
            assert_eq!(item["countsForRouting"], true);
            assert!(item.get("modelIDs").is_none() && item.get("notModels").is_none());
        } else {
            assert_eq!(item["countsForRouting"], false);
        }
    }
}
#[test]
fn standard_allowance_applies_to_new_vendor_models_by_excluding_only_core() {
    let parsed = parse(
        &limits(),
        &Account::parse(&fixture::saved()).unwrap(),
        "one",
        false,
    )
    .unwrap();
    let standard = &parsed["items"][0];
    assert!(standard.get("modelIDs").is_none());
    assert!(!standard["notModels"]
        .as_array()
        .unwrap()
        .contains(&json!("future-vendor-model")));
}
