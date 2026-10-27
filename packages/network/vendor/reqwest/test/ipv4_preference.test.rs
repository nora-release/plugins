use super::*;

#[test]
fn ipv4_preference_expires_without_extending_on_repeated_reads() {
    let mut preferences = Preferences::default();
    let now = Instant::now();
    preferences.remember("example.test:443".into(), now);
    assert!(preferences.contains("example.test:443", now + Duration::from_secs(60)));
    assert!(preferences.contains("example.test:443", now + PREFERENCE_TTL));
    assert!(!preferences.contains(
        "example.test:443",
        now + PREFERENCE_TTL + Duration::from_nanos(1)
    ));
    assert!(!preferences.0.contains_key("example.test:443"));
}

#[test]
fn ipv4_preference_is_host_and_port_specific_and_tracks_the_latest_failure() {
    let mut preferences = Preferences::default();
    let now = Instant::now();
    preferences.remember("one.test:443".into(), now);
    preferences.remember("one.test:443".into(), now + Duration::from_secs(600));
    assert!(!preferences.contains("one.test:8443", now));
    assert!(!preferences.contains("two.test:443", now));
    assert!(preferences.contains(
        "one.test:443",
        now + PREFERENCE_TTL + Duration::from_secs(1)
    ));
    assert!(!preferences.contains(
        "one.test:443",
        now + PREFERENCE_TTL + Duration::from_secs(601)
    ));
}

#[test]
fn remembering_another_host_prunes_expired_routes() {
    let mut preferences = Preferences::default();
    let now = Instant::now();
    preferences.remember("one.test:443".into(), now);
    preferences.remember(
        "two.test:443".into(),
        now + PREFERENCE_TTL + Duration::from_secs(1),
    );
    assert_eq!(preferences.0.len(), 1);
    assert!(preferences.0.contains_key("two.test:443"));
}
