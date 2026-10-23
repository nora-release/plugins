use super::*;
use crate::fixture;
#[test]
fn organization_uses_its_own_plan_and_prefers_callable_paid_memberships() {
    let mut me = fixture::me();
    assert_eq!(org(&me, ""), "personal");
    assert_eq!(plan(&me, "team"), "");
    assert_eq!(plan(&me, "personal"), "zed_student");
    me["plans_by_organization"]["team"] = json!("zed_business");
    assert_eq!(org(&me, ""), "team");
    assert_eq!(org(&me, "personal"), "personal");
    me["configuration_by_organization"] = json!({"team":{"is_zed_model_provider_enabled":false}});
    assert_eq!(org(&me, ""), "personal");
}
#[test]
fn no_paid_membership_keeps_the_saved_or_default_without_fabricating_a_plan() {
    let mut me = fixture::me();
    me["plans_by_organization"] = json!({"personal":"zed_free"});
    assert_eq!(org(&me, "personal"), "personal");
    assert_eq!(org(&me, ""), "team");
    assert_eq!(plan(&me, "team"), "");
    assert_eq!(plan_name("zed_free"), "No plan");
    assert_eq!(plan(&json!({"plan":{"plan_v3":"zed_pro"}}), ""), "zed_pro");
}
#[test]
fn account_pairs_and_tokens_remain_isolated_by_user_organization_and_access() {
    let a = Account::parse(&fixture::saved("personal")).unwrap();
    let b = Account::parse(&fixture::saved("team")).unwrap();
    assert_ne!(a.token_key(), b.token_key());
    assert_eq!(a.authorization(), "77 private-zed-access");
    assert_eq!(a.credentials()["accountId"], "77");
    let mut invalid = fixture::saved("personal");
    invalid["userID"] = json!("77\r\ninjected");
    assert!(Account::parse(&invalid).is_err());
    invalid["userID"] = json!("77");
    invalid["access"] = json!("Bearer token");
    assert!(Account::parse(&invalid).is_err());
}
