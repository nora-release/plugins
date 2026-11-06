use super::*;

struct Fixture {
    directory: std::path::PathBuf,
    limits: RateLimits,
}
impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "nora-grok-rate-limit-{}",
            nora_ai::oauth_pkce::random_hex(8)
        ));
        let limits = RateLimits::default();
        limits.initialize(&directory).unwrap();
        Self { directory, limits }
    }
}
impl std::ops::Deref for Fixture {
    type Target = RateLimits;
    fn deref(&self) -> &RateLimits {
        &self.limits
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn credentials() -> Value {
    json!({"type":"oauth","extras":{"userId":"person","teamId":"team"}})
}
fn usage(limits: &RateLimits, now: i64) -> Value {
    let mut auth = nora_provider_auth::xai_oauth_resolved_auth("fixture");
    auth.metadata
        .insert(ACCOUNT_KEY.into(), account(&credentials()).unwrap().into());
    let mut usage = json!({"items":[],"balances":[]});
    limits.apply(&auth, &mut usage, now);
    usage
}

#[test]
fn reset_headers_support_delay_http_date_epoch_seconds_and_milliseconds() {
    let now = 1_790_000_000_000;
    let expected = now + 60_000;
    let date = chrono::DateTime::from_timestamp_millis(expected)
        .unwrap()
        .to_rfc2822();
    for (key, value) in [
        ("Retry-After", "60".to_string()),
        ("retry-after", date),
        ("X-RateLimit-Reset", (expected / 1000).to_string()),
        ("x-ratelimit-reset-requests", expected.to_string()),
        ("X-RateLimit-Reset", "60".to_string()),
    ] {
        let limits = Fixture::new();
        limits.observe(
            &credentials(),
            429,
            &BTreeMap::from([(key.into(), value)]),
            now,
        );
        assert_eq!(
            usage(&limits, now)["items"][0]["resetsAt"],
            chrono::DateTime::from_timestamp_millis(expected)
                .unwrap()
                .to_rfc3339(),
            "{key}"
        );
        assert!(usage(&limits, expected)["items"]
            .as_array()
            .unwrap()
            .is_empty());
    }
}

#[test]
fn unknown_reset_uses_five_minutes_and_other_errors_do_not_clear_it() {
    let now = 1_790_000_000_000;
    for headers in [
        BTreeMap::new(),
        BTreeMap::from([
            ("retry-after".into(), "NaN".into()),
            ("x-ratelimit-reset".into(), "-2".into()),
        ]),
    ] {
        let limits = Fixture::new();
        limits.observe(&credentials(), 429, &headers, now);
        limits.observe(&credentials(), 500, &BTreeMap::new(), now + 1);
        assert_eq!(
            usage(&limits, now + HOLD_MS - 1)["items"][0]["usedPercent"],
            100.0
        );
        assert!(usage(&limits, now + HOLD_MS)["items"]
            .as_array()
            .unwrap()
            .is_empty());
    }
}

#[test]
fn api_keys_missing_identities_and_other_adapter_instances_do_not_share_a_hold() {
    let now = 1_790_000_000_000;
    let limits = Fixture::new();
    for value in [
        json!({"type":"apiKey","extras":{"userId":"person","teamId":"team"}}),
        json!({"type":"oauth","access":"unverified"}),
    ] {
        limits.observe(&value, 429, &BTreeMap::new(), now);
        assert!(usage(&limits, now)["items"].as_array().unwrap().is_empty());
    }
    limits.observe(&credentials(), 429, &BTreeMap::new(), now);
    assert_eq!(usage(&limits, now)["items"].as_array().unwrap().len(), 1);
    assert!(usage(&Fixture::new(), now)["items"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn zero_or_elapsed_reset_does_not_create_a_five_minute_hold() {
    let now = 1_790_000_000_000;
    let limits = Fixture::new();
    for headers in [
        BTreeMap::from([("retry-after".into(), "0".into())]),
        BTreeMap::from([("x-ratelimit-reset".into(), (now / 1000 - 1).to_string())]),
    ] {
        limits.observe(&credentials(), 429, &BTreeMap::new(), now - 1);
        limits.observe(&credentials(), 429, &headers, now);
        assert!(usage(&limits, now)["items"].as_array().unwrap().is_empty());
    }
}

#[test]
fn independent_writers_and_readers_do_not_see_partial_account_state() {
    let fixture = Fixture::new();
    let now = 1_790_000_000_000;
    fixture.observe(&credentials(), 429, &BTreeMap::new(), now);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let directory = &fixture.directory;
            scope.spawn(move || {
                let limits = RateLimits::default();
                limits.initialize(directory).unwrap();
                for _ in 0..10 {
                    limits.observe(&credentials(), 429, &BTreeMap::new(), now);
                    assert_eq!(usage(&limits, now)["items"][0]["usedPercent"], 100.0);
                }
            });
        }
    });
    assert_eq!(usage(&fixture, now)["items"][0]["usedPercent"], 100.0);
}
