use super::*;
use crate::fixture;
#[test]
fn aws_decimal_reset_timestamps_are_visible_for_monthly_and_trial_allowances() {
    let mut value = fixture::usage();
    value["nextDateReset"] = json!(1793491200.0);
    value["usageBreakdownList"][0]["nextDateReset"] = json!(1793491200.0);
    value["usageBreakdownList"][0]["freeTrialInfo"] = json!({
        "freeTrialStatus":"ACTIVE", "usageLimitWithPrecision":500,
        "currentUsageWithPrecision":1, "freeTrialExpiry":1800000000.125
    });
    let usage = parse(&value).unwrap();
    let resets = usage
        .windows
        .iter()
        .map(|window| window.resets_at.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(
        resets,
        [
            Some("2026-11-01T00:00:00Z"),
            Some("2027-01-15T08:00:00Z"),
            Some("2026-11-01T00:00:00Z")
        ]
    );
}

#[test]
fn monthly_and_trial_credits_are_shown_apart_and_together() {
    let mut value = fixture::usage();
    value["usageBreakdownList"][0]["freeTrialInfo"] = json!({"freeTrialStatus":"ACTIVE","usageLimitWithPrecision":500,"currentUsageWithPrecision":500,"freeTrialExpiry":1800000000});
    let usage = parse(&value).unwrap();
    assert_eq!(usage.plan.as_deref(), Some("Kiro Pro"));
    assert_eq!(usage.windows.len(), 3);
    let combined = &usage.windows[2];
    assert_eq!(combined.name, "Chat credits");
    assert_eq!(combined.balance, Some(975.0));
    assert_eq!(usage.windows[1].used, Some(100.0));
    assert!(combined
        .resets_at
        .as_deref()
        .unwrap()
        .starts_with("2027-01-15"));
    assert_eq!(combined.unit.as_deref(), Some("credits"));
}
#[test]
fn inactive_and_unknown_allowances_are_not_invented_as_exhausted_quotas() {
    let value = json!({"usageBreakdownList":[{"usageLimitWithPrecision":100,"freeTrialInfo":{"freeTrialStatus":"EXPIRED","usageLimitWithPrecision":500,"currentUsageWithPrecision":500}}]});
    let usage = parse(&value).unwrap();
    assert!(usage.windows.is_empty());
    assert!(parse(&json!({"private":"token"})).is_err());
}
#[test]
fn actual_model_fields_keep_account_default_limits_and_images() {
    let value = json!({"models":[{"modelId":"other","modelName":"Other","tokenLimits":{"maxInputTokens":200000,"maxOutputTokens":32000}},{"modelId":"auto","supportedInputTypes":["IMAGE","TEXT"]},{"modelId":"auto"},{"modelId":"invalid/id"}],"defaultModel":{"modelId":"auto"}});
    let result = models(&value).unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].id, "auto");
    assert!(result[0].images && result[0].reasoning);
    assert_eq!(
        (result[1].context, result[1].output),
        (Some(200_000), Some(32_000))
    );
    assert!(models(&json!({"models":[]})).is_err());
}
