use super::*;
#[test]
fn quota_counts_take_precedence_over_percent_and_mcp_never_blocks_chat() {
    let items = limits(
        &json!({"limits":[{"unit":3,"usage":100,"remaining":80,"percentage":90,"nextResetTime":1900000000000u64},{"unit":5,"type":"TIME_LIMIT","usage":100,"currentValue":100},{"unit":6,"usage":0,"percentage":100}]}),
    );
    assert_eq!(items[0]["usedPercent"], 20.0);
    assert_eq!(items[0]["windowSeconds"], 18000.0);
    assert_eq!(items[1]["countsForRouting"], false);
    assert_eq!(items[2]["countsForRouting"], false);
}
#[test]
fn subscription_dates_use_beijing_and_keep_renewal_distinct_from_expiry() {
    let value =
        term(&json!([{"status":"VALID","autoRenew":true,"nextRenewTime":"2026-10-20 12:00:00"}]));
    assert_eq!(value["until"], "2026-10-20T04:00:00.000Z");
    assert_eq!(value["renew"], "auto");
    let value =
        term(&json!([{"status":"VALID","autoRenew":false,"valid":"2026-09-20 ~ 2026-10-20"}]));
    assert_eq!(value["until"], "2026-10-19T16:00:00.000Z");
}
fn balance() -> Value {
    json!({"server_time":1000,"plans":[{"status":"active","plan_id":"one","name":"Start","ends_at":2000},{"status":"active","plan_id":"two","name":"Gift","ends_at":3000},{"status":"active","plan_id":"old","ends_at":999}],"balances":[{"plan_id":"one","capabilities":["model:glm-5.3-flash"],"total_units":100,"used_units":100,"remaining_units":0,"expires_at":2000},{"plan_id":"two","capabilities":["model:glm-5.3-flash"],"total_units":100,"used_units":20,"remaining_units":80,"expires_at":3000},{"plan_id":"old","capabilities":["model:expired-model"],"total_units":10,"remaining_units":10}]})
}
#[test]
fn overlapping_gift_buckets_choose_a_live_bucket_and_exclude_expired_plans() {
    let gift = gift(&balance(), true).unwrap();
    assert_eq!(gift.plan, "Start");
    assert_eq!(gift.all, vec!["glm-5.3-flash"]);
    assert_eq!(gift.live, vec!["glm-5.3-flash"]);
    assert_eq!(gift.windows[0]["countsForRouting"], false);
    assert_eq!(
        gift.windows[1]["models"],
        json!(["glm-5.3-flash", "GLM-5.3-Flash-Trial"])
    );
    assert_eq!(gift.windows[1]["usedPercent"], 20.0);
}
#[test]
fn dual_gift_windows_scope_only_trial_entries_and_keep_spent_models_listed() {
    let initial = gift(&balance(), false).unwrap();
    assert_eq!(initial.windows[1]["models"], json!(["GLM-5.3-Flash-Trial"]));
    assert_eq!(gift_id("glm-5.3"), "GLM-5.3-Trial");
    let mut value = balance();
    value["balances"][1]["remaining_units"] = json!(0);
    let gift = gift(&value, false).unwrap();
    assert!(gift.live.is_empty());
    assert_eq!(gift.all.len(), 1);
}
#[test]
fn reset_entitlements_are_read_only_counts_of_vendor_available_entries() {
    let value=resets(&json!({"fiveHourResets":[{"available":true,"expireTime":"2026-10-20"},{"available":false}],"weekResets":[{"available":true,"expireTime":1900000000}]}),true).unwrap();
    assert_eq!(value["count"], 2);
    assert_eq!(value["fiveHour"], 1);
    assert_eq!(value["weekly"], 1);
    assert_eq!(value["team"], true);
    assert!(resets(&json!({"fiveHourResets":[{"available":false}]}), false).is_none());
}
