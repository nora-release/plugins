use super::*;
#[test]
fn app_times_use_beijing_and_platform_times_use_utc() {
    assert_eq!(
        server_time(&json!("2026-10-01T12:30:00")).map(core::iso),
        Some(json!("2026-10-01T04:30:00.000Z"))
    );
    assert_eq!(
        server_time(&json!("2026-10-01")).map(core::iso),
        Some(json!("2026-09-30T16:00:00.000Z"))
    );
    let plan = token_plan(
        &json!({"planName":"Pro","expired":false,"currentPeriodEnd":"2026-10-01 12:30:00"}),
        None,
        false,
    )
    .unwrap();
    assert_eq!(plan["until"], "2026-10-01T12:30:00.000Z");
    assert!(server_time(&json!("not a time")).is_none());
}
#[test]
fn weekly_percent_is_remaining_and_not_used() {
    let result = usage(
        &json!({"current":{"planTier":3,"renewalMode":"MONTHLY"}}),
        Some(&json!({"percent":75,"resetDate":"2026-10-29"})),
        None,
        "one",
    );
    assert_eq!(result["plan"], "Pro");
    assert_eq!(result["items"][0]["usedPercent"], 25.0);
    assert_eq!(result["items"][0]["windowSeconds"], 604800.0);
    assert_eq!(result["renew"], "auto");
}
#[test]
fn token_plan_monthly_pool_is_real_and_becomes_an_aside_when_the_app_has_its_own_plan() {
    let detail = json!({"planName":"Plus","expired":false,"enableAutoRenew":true});
    let usage = json!({"monthUsage":{"items":[{"name":"month_total_token","used":2500000,"limit":"10000000"}]},"usage":{"items":[{"name":"plan_total_token","used":9,"limit":10}]}});
    let aside = token_plan(&detail, Some(&usage), true).unwrap();
    assert_eq!(aside["item"]["usedPercent"], 25.0);
    assert_eq!(aside["item"]["countsForRouting"], false);
    assert_eq!(aside["item"]["display"], "2.5M / 10M credits");
    let primary = token_plan(&detail, Some(&usage), false).unwrap();
    assert_eq!(primary["item"]["countsForRouting"], true);
    assert_eq!(primary["plan"], "Token Plan Plus");
}
#[test]
fn free_access_and_expired_token_plans_do_not_invent_an_allowance() {
    assert!(token_plan(&json!({"planName":"old","expired":true}), None, false).is_none());
    let result = usage(
        &json!({"current":null}),
        Some(&json!({"percent":100})),
        None,
        "one",
    );
    assert_eq!(result["plan"], "Free");
    assert!(result["items"].as_array().unwrap().is_empty());
    assert_eq!(credits(6_810_000.), "6.81M");
    assert_eq!(credits(4_100_000_000.), "4.1B");
}
