use super::*;
#[test]
fn native_usage_windows_keep_timezones_periods_and_model_scopes() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-22T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let usage=parse("Current session: 25% used · resets 3pm (Asia/Shanghai)\nCurrent week (all models): 40% used · resets Oct 27, 2:59pm (Asia/Shanghai)\nCurrent week (Opus): 50% used",now).unwrap();
    assert_eq!(usage["items"][0]["windowSeconds"], 18000);
    assert_eq!(usage["items"][1]["windowSeconds"], 604800);
    assert_eq!(usage["items"][2]["modelIDs"], json!(["*opus*"]));
    assert!(usage["items"][0]["resetsAt"]
        .as_str()
        .unwrap()
        .contains("2026-10-22T07:00:00"));
    assert!(usage["items"][1]["resetsAt"]
        .as_str()
        .unwrap()
        .contains("2026-10-27T06:59:00"));
}

#[test]
fn hour_only_resets_preserve_noon_midnight_and_explicit_calendar_dates() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-22T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    for (text, expected) in [
        ("12pm (Asia/Shanghai)", "2026-10-22T04:00:00+00:00"),
        ("12am (Asia/Shanghai)", "2026-10-22T16:00:00+00:00"),
        ("Oct 23 at 3pm (Asia/Shanghai)", "2026-10-23T07:00:00+00:00"),
        (
            "Oct 23, 2026 at 3pm (Asia/Shanghai)",
            "2026-10-23T07:00:00+00:00",
        ),
        (
            "Oct 23, 2026, 3pm (Asia/Shanghai)",
            "2026-10-23T07:00:00+00:00",
        ),
        (
            "Oct 23, 2:59pm (Asia/Shanghai)",
            "2026-10-23T06:59:00+00:00",
        ),
    ] {
        assert_eq!(
            reset(text, now, 604800).as_deref(),
            Some(expected),
            "{text}"
        );
    }
    assert!(reset("13pm (Asia/Shanghai)", now, 604800).is_none());
    assert!(reset("3pm (Unknown/Zone)", now, 604800).is_none());
}
#[test]
fn billing_notices_or_denials_never_turn_into_zero_usage() {
    for text in [
        "",
        "You are currently using your subscription to power your Claude Code usage",
        "private-token invalid output",
        "Not logged in · Please run /login",
    ] {
        let error = parse(text, chrono::Utc::now()).unwrap_err();
        assert!(!error.message.contains("private-token"));
    }
}
#[test]
fn unknown_model_subsets_are_displayed_without_hiding_all_chat_models() {
    let usage = parse(
        "Current week (future service): 90% used",
        chrono::Utc::now(),
    )
    .unwrap();
    assert_eq!(usage["items"][0]["countsForRouting"], false);
}
