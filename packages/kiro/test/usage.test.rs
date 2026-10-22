use super::*;
#[path = "fixture.rs"]
mod fixture;
#[test]
fn monthly_and_trial_credits_are_displayed_separately_but_routed_as_additive_credit() {
    let mut value = fixture::usage();
    value["usageBreakdownList"][0]["freeTrialInfo"] = json!({"freeTrialStatus":"ACTIVE","usageLimitWithPrecision":500,"currentUsageWithPrecision":500,"freeTrialExpiry":1800000000});
    let usage = parse(&value).unwrap();
    assert_eq!(usage["available"], true);
    assert_eq!(usage["account"]["plan"], "Kiro Pro");
    let items = usage["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    let routing = items
        .iter()
        .filter(|item| item["countsForRouting"] == true)
        .collect::<Vec<_>>();
    assert_eq!(routing.len(), 1);
    assert_eq!(routing[0]["remaining"], 975.0);
    assert_eq!(items[0]["windowSeconds"], 2592000);
    assert!(routing[0]["resetsAt"]
        .as_str()
        .unwrap()
        .starts_with("2027-01-15"));
}
#[test]
fn inactive_and_unknown_allowances_are_not_invented_as_exhausted_quotas() {
    let value = json!({"usageBreakdownList":[{"usageLimitWithPrecision":100,"freeTrialInfo":{"freeTrialStatus":"EXPIRED","usageLimitWithPrecision":500,"currentUsageWithPrecision":500}}]});
    let usage = parse(&value).unwrap();
    assert_eq!(usage["available"], false);
    assert!(usage["items"].as_array().unwrap().is_empty());
    assert!(parse(&json!({"private":"token"})).is_err());
}
#[test]
fn actual_model_fields_keep_account_default_limits_and_images() {
    let value = json!({"models":[{"modelId":"other","modelName":"Other","tokenLimits":{"maxInputTokens":200000,"maxOutputTokens":32000}},{"modelId":"auto","supportedInputTypes":["IMAGE","TEXT"]},{"modelId":"auto"},{"modelId":"invalid/id"}],"defaultModel":{"modelId":"auto"}});
    let result = models(&value).unwrap();
    assert_eq!(result["models"].as_array().unwrap().len(), 2);
    assert_eq!(result["models"][0]["id"], "auto");
    assert_eq!(result["models"][0]["input"], json!(["text", "image"]));
    assert_eq!(result["models"][1]["contextWindow"], 200000);
    assert!(models(&json!({"models":[]})).is_err());
}
