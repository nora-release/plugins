use super::*;

fn pro() -> Value {
    serde_json::from_str(include_str!("fixtures/codex_usage_pro_credits.json")).unwrap()
}
fn team() -> Value {
    serde_json::from_str(include_str!("fixtures/codex_usage_team_spend_cap.json")).unwrap()
}
fn within(plan: &str) -> Value {
    let mut value = team();
    value["plan_type"] = json!(plan);
    value["credits"]["has_credits"] = json!(false);
    value["spend_control"] = json!({"reached":false,"individual_limit":null});
    value["rate_limit_reached_type"] = json!({"type":"rate_limit_reached","details":null});
    value
}
fn no_credits() -> Value {
    let mut value = pro();
    value["credits"]["has_credits"] = json!(false);
    value["credits"]["balance"] = json!("0");
    value
}

#[test]
fn real_captured_accounts_continue_on_credits_despite_a_full_allowance_or_spend_cap() {
    for payload in [pro(), team()] {
        assert!(!held(&payload));
        let result = super::super::parse(&payload);
        assert_eq!(result["routing"]["allowed"], true);
        assert_eq!(result["routing"]["extraBalanceAvailable"], true);
        let full = result["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["usedPercent"] == 100.0 && item["includedAllowance"] == true)
            .unwrap();
        assert_eq!(full["countsForRouting"], false);
        assert_eq!(full["usedPercent"], 100.0);
    }
}

#[test]
fn an_explicitly_spent_balance_overrides_the_has_credits_flag() {
    for balance in [
        json!("0"),
        json!(" -0.5 "),
        json!(0),
        json!(-1),
        json!("NaN"),
    ] {
        let mut payload = pro();
        payload["credits"]["balance"] = balance;
        assert!(held(&payload));
        let result = super::super::parse(&payload);
        assert_eq!(result["routing"]["extraBalanceAvailable"], false);
        payload["credits"]["unlimited"] = json!(true);
        assert!(!held(&payload));
    }
    for balance in [json!("1.25"), Value::Null, json!("unknown")] {
        let mut payload = pro();
        payload["credits"]["balance"] = balance;
        assert!(!held(&payload));
    }
}
#[test]
fn gate_matches_the_upstream_credit_allowed_and_workspace_matrix() {
    let mut cases = vec![("credits spent", no_credits(), true)];
    let mut value = no_credits();
    value["credits"]["unlimited"] = json!(true);
    cases.push(("unlimited", value, false));
    let mut value = pro();
    value.as_object_mut().unwrap().remove("credits");
    cases.push(("credits absent", value, true));
    let mut value = pro();
    value["credits"]["overage_limit_reached"] = json!(true);
    cases.push(("credits beat overage", value, false));
    let mut value = pro();
    value["spend_control"]["reached"] = json!(true);
    cases.push(("credits beat spend cap", value, false));
    let mut value = no_credits();
    value["rate_limit"]["allowed"] = json!(true);
    cases.push(("allowed despite limit", value, false));
    let mut value = no_credits();
    value["rate_limit"]
        .as_object_mut()
        .unwrap()
        .remove("allowed");
    cases.push(("allowed absent", value, false));
    let mut value = no_credits();
    value["rate_limit"]["limit_reached"] = json!(false);
    cases.push(("denied despite flag", value, true));
    for plan in [
        "business",
        "k12",
        "law",
        "sci",
        "finserv",
        "enterprise_cbp_trial",
        "enterprise_cbp_view_only",
    ] {
        cases.push((plan, within(plan), false));
    }
    for plan in ["pro", "team", "self_serve_business_prolite", "unknown"] {
        cases.push((plan, within(plan), true));
    }
    let mut value = within("business");
    value["spend_control"]["reached"] = json!(true);
    cases.push(("workspace cap", value, true));
    let mut value = within("business");
    value["rate_limit_reached_type"]["type"] = json!("workspace_owner_usage_limit_reached");
    cases.push(("workspace owner limit", value, true));
    let mut value = within("business");
    value["credits"]["overage_limit_reached"] = json!(true);
    cases.push(("overage exhausted", value, true));
    let mut value = within("business");
    value["credits"]
        .as_object_mut()
        .unwrap()
        .remove("overage_limit_reached");
    cases.push(("overage unknown", value, true));
    let mut value = team();
    value["credits"]["has_credits"] = json!(false);
    cases.push(("team owner limit", value, true));
    for (name, payload, want) in cases {
        assert_eq!(held(&payload), want, "{name}");
        assert_eq!(
            super::super::parse(&payload)["routing"]["allowed"],
            !want,
            "{name}"
        );
    }
}
#[test]
fn provider_denial_does_not_invent_a_reset_or_replace_the_displayed_percentage() {
    let mut payload = no_credits();
    payload["rate_limit"]["primary_window"]["used_percent"] = json!(40);
    let result = super::super::parse(&payload);
    assert_eq!(result["routing"]["allowed"], false);
    assert!(result["routing"].get("resetsAt").is_none());
    assert_eq!(result["items"][0]["usedPercent"], 40.0);
    assert_eq!(result["items"][0]["countsForRouting"], true);
    payload["rate_limit"] = json!({"allowed":false});
    assert_eq!(super::super::parse(&payload)["routing"]["allowed"], false);
}
#[test]
fn non_exhausted_windows_still_supply_routing_usage_and_user_cap_identity() {
    let mut payload = pro();
    payload["rate_limit"]["primary_window"]["used_percent"] = json!(96);
    let result = super::super::parse(&payload);
    assert_eq!(result["items"][0]["usedPercent"], 96.0);
    assert_eq!(result["items"][0]["countsForRouting"], true);
    assert_eq!(result["items"][0]["includedAllowance"], true);
    assert_eq!(result["routing"]["supportsExtraBalance"], true);
}
