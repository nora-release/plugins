use super::*;
#[test]
fn plan_prefixes_keep_legacy_pro_and_goat_distinct() {
    assert_eq!(plan("individual_pro_v1_monthly"), ("Pro", 80.));
    assert_eq!(plan("individual-goat-monthly"), ("GOAT", 70.));
    assert_eq!(plan("individual-go-monthly"), ("Go", 10.));
}
#[test]
fn cancellation_at_period_end_retains_active_subscription() {
    let value=subscription(&json!({"data":{"planId":"individual-provider-monthly","status":"active","cancelAtPeriodEnd":true,"currentPeriodEnd":1_900_000_000}})).unwrap();
    assert_eq!(value["plan"], "Provider");
    assert_eq!(value["renew"], "off");
    assert_eq!(value["until"], 1_900_000_000_000u64);
    assert!(subscription(&json!({"success":false})).is_none());
    assert_eq!(
        subscription(&json!({"data":{"planId":"individual-go","status":"canceled"}})).unwrap()
            ["plan"],
        "No plan"
    );
}
#[test]
fn credits_use_real_extra_pools_and_period_end_instead_of_guessing_a_percent() {
    let credits = json!({"credits":{"monthlyCredits":5,"purchasedCredits":2,"freeCredits":1},"windowLimits":{"fiveHour":{"used":3,"cap":10,"resetAt":1_900_000_000}}});
    let sub = json!({"id":"individual-go","plan":"Go","until":1_900_000_000_000u64});
    let value = usage(Some(&credits), Some(&sub), "one");
    assert_eq!(value["items"][0]["usedPercent"], 30.0);
    assert_eq!(value["items"][1]["used"], 5.0);
    assert_eq!(value["items"][1]["limit"], 13.0);
    assert!(value["items"][1]["resetsAt"].as_str().is_some());
    assert!(extra(&credits));
}
#[test]
fn money_without_a_known_plan_stays_a_balance_and_outages_keep_the_plan() {
    let value = usage(
        Some(&json!({"credits":{"purchasedCredits":"2.5"}})),
        None,
        "one",
    );
    assert_eq!(value["balance"], "$2.50");
    assert!(value["items"].as_array().unwrap().is_empty());
    let value = usage(None, Some(&json!({"plan":"Go"})), "one");
    assert_eq!(value["available"], false);
    assert_eq!(value["plan"], "Go");
}
