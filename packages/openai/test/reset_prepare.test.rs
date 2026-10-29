use super::*;

fn now() -> i64 {
    DateTime::parse_from_rfc3339("2026-10-24T15:20:00Z")
        .unwrap()
        .timestamp_millis()
}
fn usage(five: f64, back: Option<i64>, week: f64) -> Value {
    let now = now();
    json!({"available":true,"routing":{"allowed":five<100.0&&week<100.0},
        "items":[
            {"id":"rate_limit:primary_window","includedAllowance":true,"windowSeconds":18000,"usedPercent":five,"resetsAt":back.map(iso)},
            {"id":"rate_limit:secondary_window","includedAllowance":true,"windowSeconds":604800,"usedPercent":week,"resetsAt":iso(now+3*86400_000)}],
        "resetCredits":{"available":true,"totalAvailable":2,"grants":[
            {"id":"later","status":"available","expiresAt":iso(now+24*3600_000)},
            {"id":"first","status":"available","expiresAt":iso(now+175*MINUTE)}]}})
}
#[test]
fn only_genuine_future_weekly_exhaustion_is_eligible() {
    let now = now();
    for (five, week, expected) in [(100.0, 99.5, false), (0.0, 100.0, true), (0.0, 0.0, false)] {
        let result = reading(&usage(five, Some(now + 3600_000), week), now).unwrap();
        assert_eq!(result["weekly"]["exhausted"] == true, expected);
    }
    for field in ["aside", "past", "missing", "credits"] {
        let mut value = usage(0.0, Some(now + 3600_000), 100.0);
        match field {
            "aside" => value["items"][1]["aside"] = json!(true),
            "past" => value["items"][1]["resetsAt"] = json!(iso(now - 1)),
            "missing" => {
                value["items"][1]
                    .as_object_mut()
                    .unwrap()
                    .remove("resetsAt");
            }
            "credits" => value["items"][1]["countsForRouting"] = json!(false),
            _ => unreachable!(),
        }
        assert!(
            reading(&value, now).unwrap().get("weekly").is_none(),
            "{field}"
        );
    }
}
#[test]
fn expiring_reset_waits_until_lead_unless_held_past_it_and_chooses_earliest() {
    let now = now();
    let lead = now + 145 * MINUTE;
    for (five, back, week, expected) in [
        (100.0, Some(now + 125 * MINUTE), 32.0, lead),
        (100.0, Some(now + 140 * MINUTE), 32.0, lead),
        (100.0, Some(now + 150 * MINUTE), 32.0, now),
        (100.0, Some(now + 220 * MINUTE), 32.0, now),
        (100.0, None, 32.0, now),
        (40.0, Some(now + 100 * MINUTE), 100.0, now),
        (60.0, Some(now + 109 * MINUTE), 32.0, lead),
    ] {
        let result = reading(&usage(five, back, week), now).unwrap();
        assert_eq!(result["offerID"], "first");
        assert_eq!(at(&result["expiring"]["dueAt"]), Some(expected));
    }
    let mut fresh = usage(0.0, Some(now + 100 * MINUTE), 0.0);
    fresh["items"]
        .as_array_mut()
        .unwrap()
        .push(json!({"includedAllowance":true,"aside":true,"usedPercent":90}));
    assert_eq!(reading(&fresh, now).unwrap()["windowsUsed"], false);
}
#[test]
fn invalid_expired_redeemed_future_grants_are_never_spendable() {
    let now = now();
    let parsed = json!({"available":true,"totalAvailable":5,"grants":[
        {"id":"expired","status":"available","expiresAt":iso(now)},
        {"id":"redeemed","status":"available","redeemedAt":iso(now-100)},
        {"id":"pending","status":"pending","expiresAt":iso(now+1)},
        {"id":"future","status":"available","grantedAt":iso(now+1)},
        {"id":"good","status":"available","expiresAt":iso(now+1000)}]});
    assert_eq!(
        earliest(&parsed, now),
        Some(("good".into(), Some(now + 1000)))
    );
}
#[test]
fn polling_frequency_matches_the_expiry_windows_without_network_guesses() {
    let now = now();
    for (expires, next) in [(180, 30), (90, 30), (70, 10), (50, 5)] {
        let mut value = usage(20.0, Some(now + 1000), 40.0);
        value["resetCredits"]["grants"][1]["expiresAt"] = json!(iso(now + expires * MINUTE));
        assert_eq!(
            at(&reading(&value, now).unwrap()["nextReadAt"]),
            Some(now + next * MINUTE)
        );
    }
    let mut value = usage(0.0, None, 0.0);
    value["resetCredits"]["grants"] = json!([]);
    assert_eq!(
        at(&reading(&value, now).unwrap()["nextReadAt"]),
        Some(now + 6 * 60 * MINUTE)
    );
    assert!(reading(&json!({}), now).is_err());
}
