use super::*;
use crate::fixture;
#[test]
fn missing_zero_share_on_quota_plan_means_exhausted_week() {
    let account = Account::parse(&fixture::saved()).unwrap();
    let rows = models::parse(&fixture::configs()).unwrap();
    let value = parse(&fixture::me(), &account, "one", &rows).unwrap();
    let items = value["items"].as_array().unwrap();
    assert_eq!(items[0]["usedPercent"], 0.0);
    assert_eq!(items[1]["usedPercent"], 100.0);
    assert_eq!(items[0]["windowSeconds"], 86400);
    assert_eq!(items[1]["windowSeconds"], 604800);
    assert_eq!(items[1]["resetsAt"], "2030-01-01T00:00:00.000Z");
    let ids = items[1]["modelIDs"].as_array().unwrap();
    assert!(!ids.iter().any(|v| v.as_str().unwrap().starts_with("swe-2")));
    assert_eq!(items[1]["countsForRouting"], true);
    assert_eq!(value["balances"][0]["balance"], 1.25);
    assert_eq!(value["balances"][0]["countsForRouting"], false);
}
#[test]
fn hidden_quotas_and_nonquota_missing_shares_are_not_invented() {
    let account = Account::parse(&fixture::saved()).unwrap();
    let rows = models::parse(&fixture::configs()).unwrap();
    let mut value = fixture::me();
    value["userStatus"]["planStatus"]["planInfo"]["hideDailyQuota"] = json!(true);
    value["userStatus"]["planStatus"]["planInfo"]["billingStrategy"] = json!("OTHER");
    value["userStatus"]["planStatus"]["acuConsumed"] = json!(12.5);
    value["userStatus"]["planStatus"]["acuLimit"] = json!(50);
    let value = parse(&value, &account, "one", &rows).unwrap();
    assert_eq!(value["items"].as_array().unwrap().len(), 1);
    assert_eq!(value["items"][0]["id"], "acus");
    assert_eq!(value["items"][0]["usedPercent"], 25.0);
    assert_eq!(value["items"][0]["countsForRouting"], false);
}
#[test]
fn bad_percentages_and_missing_plan_do_not_look_like_available_zero_usage() {
    let account = Account::parse(&fixture::saved()).unwrap();
    let mut value = fixture::me();
    value["userStatus"]["planStatus"]["dailyQuotaRemainingPercent"] = json!("bad");
    assert!(parse(&value, &account, "one", &[]).is_err());
    assert!(parse(&json!({}), &account, "one", &[]).is_err());
}
