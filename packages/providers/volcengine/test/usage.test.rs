use super::*;

#[test]
fn only_official_ark_plan_endpoints_select_an_action_and_region() {
    for (endpoint, expected) in [
        (
            "https://ark.cn-beijing.volces.com/api/coding/v3",
            Some(("GetCodingPlanUsage", "Coding Plan", "cn-beijing")),
        ),
        (
            "https://ark.cn-shanghai.volces.com/api/plan",
            Some(("GetAFPUsage", "Agent Plan", "cn-shanghai")),
        ),
        (
            "https://ark.ap-southeast.volces.com/api/plan/v3/",
            Some(("GetAFPUsage", "Agent Plan", "ap-southeast")),
        ),
        ("https://ark.cn-beijing.volces.com/api/v3", None),
        ("https://relay.example/api/coding", None),
        ("https://ark.cn-beijing.volces.com.evil/api/coding", None),
        ("https://ark.cn-beijing.volces.com/api/codingx", None),
        ("http://ark.cn-beijing.volces.com/api/coding", None),
        ("https://ark.cn-beijing.volces.com:8443/api/coding", None),
        ("https://user@ark.cn-beijing.volces.com/api/coding", None),
    ] {
        assert_eq!(
            plan(Some(endpoint)).map(|p| (p.action, p.label, p.region)),
            expected.map(|(a, p, r)| (a, p, r.to_owned())),
            "{endpoint}"
        );
    }
}
#[test]
fn coding_plan_percentages_and_reset_seconds_are_not_token_balances() {
    let plan = plan(Some("https://ark.cn-beijing.volces.com/api/coding/v3")).unwrap();
    let payload = json!({"Result":{"Status":"ok","QuotaUsage":[
        {"Level":"session","Percent":10.5,"ResetTimestamp":1758100000},
        {"Level":"weekly","Percent":20.25,"ResetTimestamp":1758200000},
        {"Level":"monthly","Percent":"30.75","ResetTimestamp":1758300000},
        {"Level":"daily","Percent":1}
    ]}});
    let value = parse(&payload, &plan).unwrap();
    assert_eq!(value["account"]["plan"], "Coding Plan");
    assert_eq!(value["items"].as_array().unwrap().len(), 3);
    assert_eq!(value["items"][0]["usedPercent"], 10.5);
    assert_eq!(value["items"][0]["windowSeconds"], 18000.0);
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(value["items"][0]["resetsAt"].as_str().unwrap())
            .unwrap()
            .timestamp(),
        1758100000
    );
    assert_eq!(value["items"][2]["usedPercent"], 30.75);
    assert!(value["items"][0].get("used").is_none());
    assert!(value["balances"].as_array().unwrap().is_empty());
}
#[test]
fn agent_plan_quota_total_percent_and_all_reset_formats_match_actual_shapes() {
    let plan = plan(Some("https://ark.cn-shanghai.volces.com/api/plan/v3")).unwrap();
    let value = parse(
        &json!({"Result":{"Tier":"Pro",
            "AFPFiveHour":{"Quota":100,"Used":42,"ResetTime":1790000000000u64},
            "AFPDaily":{"Quota":500,"Used":400},
            "AFPWeekly":{"Used":"30","Total":"120","ResetTime":"2026-10-12T00:00:00Z"},
            "AFPMonthly":{"Percent":12.5,"ResetTimestamp":1790000000}
        }}),
        &plan,
    )
    .unwrap();
    assert_eq!(value["account"]["plan"], "Agent Plan Pro");
    let items = value["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["used"], 42.0);
    assert_eq!(items[0]["limit"], 100.0);
    assert_eq!(items[0]["usedPercent"], 42.0);
    assert_eq!(items[0]["resetsAt"], items[2]["resetsAt"]);
    assert_eq!(items[1]["usedPercent"], 25.0);
    assert_eq!(items[1]["resetsAt"], "2026-10-12T00:00:00+00:00");
    assert_eq!(items[2]["usedPercent"], 12.5);
}
#[test]
fn absent_or_zero_quotas_are_unknown_not_an_empty_successful_plan() {
    let plan = plan(Some("https://ark.cn-beijing.volces.com/api/plan")).unwrap();
    for result in [
        json!({}),
        json!({"AFPFiveHour":{"Quota":0,"Used":0}}),
        json!({"AFPFiveHour":{"Quota":100}}),
    ] {
        assert!(parse(&json!({"Result":result}), &plan).is_err());
    }
}
