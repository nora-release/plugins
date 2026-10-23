use super::*;
#[test]
fn live_catalog_uses_actual_ids_capabilities_and_discount_multipliers() {
    let rows=models(&json!({"data":{"categories":[{"models":[{"name":"real-model","display_name":"Actual Name","tags":["vision"],"context_window":123456,"max_tokens":1000,"billing_multiplier":1,"billing_effective_multiplier":0.5}]}]}})).unwrap();
    assert_eq!(rows[0]["id"], "real-model");
    assert_eq!(rows[0]["contextWindow"], 123456);
    assert_eq!(rows[0]["input"], json!(["text", "image"]));
    assert_eq!(rows[0]["info"]["creditMultiplier"], 0.5);
    assert_eq!(rows[0]["info"]["previousCreditMultiplier"], 1.0);
    assert_eq!(rows[0]["cost"]["input"], 0);
}
#[test]
fn incomplete_grant_history_does_not_fabricate_remaining_percent() {
    let result = usage(
        &json!({"available_points":9000}),
        &Value::Null,
        &json!({"items":[{"points":3000}]}),
        "one",
    )
    .unwrap();
    assert_eq!(result["balance"], "9000 credits");
    assert!(result["items"].as_array().unwrap().is_empty());
}
#[test]
fn only_unexpired_vendor_grants_form_a_real_routing_window() {
    let result=usage(&json!({"available_points":2000}),&Value::Null,&json!({"items":[{"points":5000,"expire_at":"2099-01-01T00:00:00Z"},{"points":9000,"expire_at":"2000-01-01T00:00:00Z"},{"points":-500}]}),"one").unwrap();
    assert_eq!(result["items"][0]["usedPercent"], 60.0);
    assert_eq!(result["items"][0]["used"], 3000.0);
    assert_eq!(result["items"][0]["runsOut"], 2000.0);
}
#[test]
fn a_missing_balance_is_not_reported_as_zero() {
    assert!(usage(&json!({}), &Value::Null, &Value::Null, "one").is_err());
}
#[test]
fn daily_reward_detection_ignores_consumption_and_old_rewards() {
    let today = "2030-01-01";
    assert!(grants(
        &json!({"points":20,"created_at":"2030-01-01 10:00:00","biz_type":"daily_grant"}),
        today
    ));
    assert!(!grants(
        &json!({"points":-20,"created_at":"2030-01-01"}),
        today
    ));
    assert!(!grants(
        &json!({"points":20,"created_at":"2029-12-31","biz_type":"daily_grant"}),
        today
    ));
}
