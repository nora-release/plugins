use super::*;

#[test]
fn code_completions_and_unlimited_requests_do_not_restrict_chat_routing() {
    let usage = parse(&json!({"quota_snapshots": {
        "chat": {"entitlement": 100, "quota_remaining": 25},
        "completions": {"entitlement": 100, "quota_remaining": 0},
        "premium_interactions": {"unlimited": true, "has_quota": true}
    }}));
    let items = usage["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    for item in items {
        assert_eq!(item["countsForRouting"], item["id"] == "chat");
    }
}

#[test]
fn paid_overage_only_excludes_personal_premium_allowances() {
    for (plan, overage, expected) in [
        ("individual", true, false),
        ("individual", false, true),
        ("business", true, true),
        ("enterprise", true, true),
    ] {
        let usage = parse(&json!({
            "copilot_plan": plan,
            "quota_reset_date": "2026-11-01",
            "quota_snapshots": {"premium_interactions": {
                "entitlement": 100,
                "quota_remaining": 0,
                "overage_permitted": overage
            }}
        }));
        let item = &usage["items"][0];
        assert_eq!(
            item["countsForRouting"], expected,
            "{plan}, overage={overage}"
        );
        assert_eq!(item["usedPercent"], 100.0);
        assert!(timestamp(&item["resetsAt"]).is_some());
    }
}

#[test]
fn an_exhausted_organization_pool_counts_but_zero_entitlement_does_not() {
    for unlimited in [true, false] {
        let usage = parse(&json!({
            "copilot_plan": "enterprise",
            "quota_snapshots": {"premium_interactions": {
                "unlimited": unlimited,
                "has_quota": false,
                "entitlement": "0",
                "quota_remaining": "0",
                "overage_permitted": true
            }}
        }));
        if unlimited {
            assert_eq!(usage["items"][0]["countsForRouting"], true);
            assert_eq!(usage["items"][0]["usedPercent"], 100.0);
        } else {
            assert!(usage["items"].as_array().unwrap().is_empty());
            assert!(usage["balances"].as_array().unwrap().is_empty());
        }
    }
}

#[test]
fn free_accounts_do_not_show_a_premium_allowance_they_have_never_had() {
    let usage = parse(&json!({
        "copilot_plan": "free",
        "access_type_sku": "free_limited_copilot",
        "quota_snapshots": {
            "chat": {"entitlement":"200", "percent_remaining":98.7, "has_quota":false},
            "completions": {"entitlement":"4000", "percent_remaining":100, "has_quota":false},
            "premium_interactions": {"entitlement":"0", "percent_remaining":0, "has_quota":false}
        }
    }));
    let items = usage["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], "chat");
    assert!((items[0]["usedPercent"].as_f64().unwrap() - 1.3).abs() < 1e-9);
    assert_eq!(items[1]["id"], "completions");
    assert!(usage["balances"]
        .as_array()
        .unwrap()
        .iter()
        .all(|balance| balance["id"] != "premium_interactions"));
}

#[test]
fn finite_and_exhausted_pooled_allowances_keep_their_monthly_period() {
    for quota in [
        json!({"entitlement":100,"quota_remaining":50}),
        json!({"entitlement":"100","quota_remaining":"50"}),
        json!({"unlimited":true,"has_quota":false}),
    ] {
        let usage = parse(&json!({"quota_snapshots":{"premium_interactions":quota}}));
        assert_eq!(usage["items"][0]["windowSeconds"], 30 * 24 * 3600);
    }
    let usage = parse(&json!({"quota_snapshots":{"chat":{"unlimited":true}}}));
    assert!(usage["items"][0].get("windowSeconds").is_none());
}

#[test]
fn plan_names_follow_github_entitlement_and_education_sku() {
    for (plan, sku, expected) in [
        ("individual", "", "Pro"),
        ("individual_pro", "", "Pro+"),
        ("individual_max", "", "Max"),
        ("individual_edu", "", "Education"),
        ("business", "", "Business"),
        ("enterprise", "", "Enterprise"),
        ("individual", "free_educational_quota", "Education"),
        ("individual", "free_limited_copilot", "Free"),
        ("future_plan", "", "Future_plan"),
    ] {
        let usage = parse(&json!({"copilot_plan":plan,"access_type_sku":sku}));
        assert_eq!(usage["account"]["plan"], expected);
        assert_eq!(usage["plan"], expected);
    }
}
