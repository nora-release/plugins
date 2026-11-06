use super::*;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 4, 10, 0, 0, 0).unwrap()
}
fn plan() -> Value {
    json!({"TeamId":"team-a","ProductType":"enterprise",
    "PackageInfo":{"TotalQuota":"1000000","TotalUsed":"100000","CycleQuota":"1000000","CurrentCycle":1,"CycleUnit":"month","StartTime":"2026-04-01T00:00:00Z","ExpireTime":"2027-04-01T00:00:00Z"},
    "TokenSummary":{"CycleStartTime":"2026-04-01T00:00:00+08:00","CycleEndTime":"2026-05-01T00:00:00+08:00"}})
}
#[test]
fn only_enterprise_tokenhub_endpoints_select_a_cloud_region() {
    for (base, expected) in [
        (
            "https://tokenhub.tencentmaas.com/plan/v3",
            Some("ap-guangzhou"),
        ),
        (
            "https://tokenhub.tencentmaas.com/plan/anthropic",
            Some("ap-guangzhou"),
        ),
        (
            "https://TokenHub-Intl.tencentmaas.com/plan/v3/",
            Some("ap-singapore"),
        ),
        ("https://tokenhub.tencentmaas.com/v1", None),
        ("https://api.lkeap.cloud.tencent.com/plan/v3", None),
        ("https://relay.example/plan/v3", None),
        ("https://tokenhub.tencentmaas.com.evil/plan/v3", None),
        ("https://tokenhub.tencentmaas.com/planx/v3", None),
        ("http://tokenhub.tencentmaas.com/plan/v3", None),
        ("https://tokenhub.tencentmaas.com:8443/plan/v3", None),
        ("https://user@tokenhub.tencentmaas.com/plan/v3", None),
    ] {
        assert_eq!(region(Some(base)), expected, "{base}");
    }
}
#[test]
fn actual_team_package_and_cycle_summary_preserve_credits_tokens_and_timezones() {
    let mut plan = plan();
    let value = parse(&plan, "team-a", "Month", now()).unwrap();
    assert_eq!(value["used"], 100000.0);
    assert_eq!(value["limit"], 1000000.0);
    assert_eq!(value["usedPercent"], 10.0);
    assert_eq!(value["unit"], "credits");
    assert_eq!(value["windowSeconds"], 30.0 * 86400.0);
    assert_eq!(value["resetsAt"], "2026-05-01T00:00:00+08:00");
    plan["ProductType"] = json!("enterprise-auto");
    assert_eq!(
        parse(&plan, "team-a", "Month", now()).unwrap()["unit"],
        "tokens"
    );
}
#[test]
fn absent_summary_uses_current_month_and_never_exceeds_package_expiry() {
    let mut plan = plan();
    plan.as_object_mut().unwrap().remove("TokenSummary");
    assert_eq!(
        parse(&plan, "team-a", "Month", now()).unwrap()["resetsAt"],
        "2026-05-01T00:00:00+00:00"
    );
    let july = Utc.with_ymd_and_hms(2026, 7, 15, 0, 0, 0).unwrap();
    assert_eq!(
        parse(&plan, "team-a", "Month", july).unwrap()["resetsAt"],
        "2026-08-01T00:00:00+00:00"
    );
    plan["PackageInfo"]["ExpireTime"] = json!("2026-07-20T00:00:00Z");
    assert_eq!(
        parse(&plan, "team-a", "Month", july).unwrap()["resetsAt"],
        "2026-07-20T00:00:00+00:00"
    );
}
#[test]
fn month_overflow_uses_the_reference_go_adddate_semantics() {
    let start = DateTime::parse_from_rfc3339("2026-01-31T10:30:00+08:00").unwrap();
    assert_eq!(
        month(start, 1).unwrap().to_rfc3339(),
        "2026-03-03T10:30:00+08:00"
    );
}
#[test]
fn total_quota_is_the_fallback_and_missing_totals_do_not_become_zero_usage() {
    let mut plan = plan();
    plan["PackageInfo"]["CycleQuota"] = Value::Null;
    plan["PackageInfo"]["TotalUsed"] = json!(2000000);
    let value = parse(&plan, "team-a", "Month", now()).unwrap();
    assert_eq!(value["usedPercent"], 100.0);
    assert_eq!(value["limit"], 1000000.0);
    for package in [
        json!({}),
        json!({"TotalQuota":0,"TotalUsed":1}),
        json!({"TotalQuota":10}),
    ] {
        assert!(parse(&json!({"PackageInfo":package}), "a", "Month", now()).is_none());
    }
}
#[test]
fn stopped_plans_keep_the_real_status_and_stop_reason() {
    assert_eq!(
        status(&json!({"Status":"enable","StopReason":"NORMAL"})),
        ""
    );
    assert_eq!(
        status(&json!({"Status":"isolate","StopReason":"arrears"})),
        " · isolate, arrears"
    );
    assert_eq!(
        status(&json!({"Status":"disabled","StopReason":"none"})),
        " · disabled"
    );
}
