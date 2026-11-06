use super::*;
use crate::fixture::Directory;

fn credentials(subject: &str, scope: &str) -> Value {
    json!({"type":"oauth","accountIdentity":{"issuer":"https://claude.ai","subject":subject,"scope":scope},"access":"private-token","refresh":"private-refresh"})
}
fn event(kind: &str, used: f64, reset: i64) -> Value {
    json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed","rateLimitType":kind,"utilization":used,"resetsAt":reset}})
}
const NOW: i64 = 1_800_000_000_000;
fn cache(root: &Path) -> Cache {
    Cache::for_credentials(root, &credentials("account-a", "org-a")).unwrap()
}

#[test]
fn unified_windows_and_primary_fallback_match_claude_codes_allowance_events() {
    let parsed = parse(
        &json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed","rateLimitType":"five_hour","utilization":0.42,"resetsAt":NOW/1000+18000,"unifiedWindows":{"seven_day":{"utilization":0.17,"resetsAt":NOW/1000+604800},"unrecognized":{"utilization":0.1}}}}),
        NOW,
    );
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed["five_hour"]["used"], 0.42);
    assert_eq!(parsed["seven_day"]["used"], 0.17);
    let parsed = parse(
        &json!({"type":"rate_limit_event","rate_limit_info":{"status":"rejected","rateLimitType":"seven_day_sonnet","resetsAt":NOW/1000+10}}),
        NOW,
    );
    assert_eq!(parsed["seven_day_sonnet"]["used"], 1.0);
    let parsed = parse(
        &json!({"type":"rate_limit_event","rate_limit_info":{"rateLimitType":"five_hour","utilization":0.9,"unifiedWindows":{"five_hour":{"utilization":0.2}}}}),
        NOW,
    );
    assert_eq!(parsed["five_hour"]["used"], 0.2);
}

#[test]
fn malformed_unknown_and_reset_windows_never_invent_available_allowance() {
    for value in [
        json!({"type":"rate_limit_event","rate_limit_info":{}}),
        event("unknown", 0.1, NOW / 1000 + 5),
        event("five_hour", 1.1, NOW / 1000 + 5),
        event("five_hour", 0.1, NOW / 1000),
        json!({"type":"rate_limit_event","rate_limit_info":{"rateLimitType":"five_hour","unifiedWindows":{"five_hour":{}}}}),
    ] {
        assert!(parse(&value, NOW).is_empty());
    }
}

#[test]
fn per_window_ttl_and_reset_filtering_keep_partial_updates_without_refreshing_old_windows() {
    let directory = Directory::new();
    let cache = cache(&directory.0);
    cache
        .write_at(&event("five_hour", 0.4, NOW / 1000 + 10000), NOW)
        .unwrap();
    cache
        .write_at(&event("seven_day", 0.2, NOW / 1000 + 604800), NOW)
        .unwrap();
    cache
        .write_at(
            &event("five_hour", 0.5, NOW / 1000 + 10000),
            NOW + 30 * 60_000,
        )
        .unwrap();
    let usage = cache.usage_at(NOW + 70 * 60_000).unwrap();
    assert_eq!(usage["items"].as_array().unwrap().len(), 1);
    assert_eq!(usage["items"][0]["id"], "five_hour");
    assert_eq!(usage["readAt"], NOW + 30 * 60_000);
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(usage["observedAt"].as_str().unwrap())
            .unwrap()
            .timestamp_millis(),
        NOW + 30 * 60_000
    );
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(usage["items"][0]["observedAt"].as_str().unwrap())
            .unwrap()
            .timestamp_millis(),
        NOW + 30 * 60_000
    );
    assert!(cache.usage_at(NOW + 90 * 60_000).is_none());
    cache
        .write_at(&event("seven_day", 0.9, NOW / 1000 + 1), NOW)
        .unwrap();
    assert!(!cache.read_at(NOW + 1000).unwrap().contains_key("seven_day"));
}

#[test]
fn older_arriving_observations_cannot_erase_newer_windows() {
    let directory = Directory::new();
    let cache = cache(&directory.0);
    cache
        .write_at(&event("five_hour", 0.9, NOW / 1000 + 10000), NOW + 1000)
        .unwrap();
    cache
        .write_at(&event("five_hour", 0.1, NOW / 1000 + 10000), NOW)
        .unwrap();
    cache
        .write_at(&event("seven_day", 0.2, NOW / 1000 + 10000), NOW)
        .unwrap();
    let usage = cache.usage_at(NOW + 1000).unwrap();
    assert_eq!(usage["items"].as_array().unwrap().len(), 2);
    assert_eq!(usage["items"][0]["usedPercent"], 90.0);
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(usage["items"][0]["observedAt"].as_str().unwrap())
            .unwrap()
            .timestamp_millis(),
        NOW + 1000
    );
}

#[test]
fn cache_is_identity_scoped_across_organizations_and_never_includes_secrets() {
    let directory = Directory::new();
    let original = cache(&directory.0);
    original
        .write_at(&event("seven_day_opus", 0.9, NOW / 1000 + 1000), NOW)
        .unwrap();
    let usage = original.usage_at(NOW).unwrap();
    assert_eq!(usage["items"][0]["modelIDs"], json!(["*opus*"]));
    for value in [
        credentials("account-b", "org-a"),
        credentials("account-a", "org-b"),
    ] {
        assert!(Cache::for_credentials(&directory.0, &value)
            .unwrap()
            .usage_at(NOW)
            .is_none());
    }
    for key in ["type", "authMethod"] {
        let mut api = credentials("account-a", "org-a");
        api[key] = json!("apiKey");
        assert!(Cache::for_credentials(&directory.0, &api).is_none());
    }
    assert!(Cache::for_credentials(
        &directory.0,
        &json!({"type":"oauth","email":"person@example.test"})
    )
    .is_none());
    let bytes = std::fs::read_to_string(original.state_path()).unwrap();
    for secret in ["private-token", "private-refresh", "account-a", "org-a"] {
        assert!(!bytes.contains(secret));
    }
}

#[test]
fn concurrent_independent_writers_merge_windows_in_one_atomic_private_document() {
    let directory = Directory::new();
    let original = cache(&directory.0);
    let threads = KINDS
        .into_iter()
        .map(|(kind, _, _, _, _)| {
            let cache = original.clone();
            std::thread::spawn(move || {
                for step in 0..10 {
                    cache
                        .write_at(
                            &event(kind, step as f64 / 10.0, NOW / 1000 + 10000),
                            NOW + step,
                        )
                        .unwrap();
                }
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        thread.join().unwrap();
    }
    let usage = original.usage_at(NOW + 100).unwrap();
    assert_eq!(usage["items"].as_array().unwrap().len(), 5);
    assert!(usage["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["usedPercent"] == 90.0));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(original.state_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert!(!std::fs::read_dir(original.directory)
        .unwrap()
        .flatten()
        .any(|entry| entry.file_name().to_string_lossy().ends_with(".tmp")));
}

#[test]
fn absent_corrupt_wrong_account_or_oversized_cache_reads_are_empty_and_do_not_create_files() {
    let directory = Directory::new();
    let cache = cache(&directory.0);
    assert!(cache.usage_at(NOW).is_none());
    assert!(!cache.directory.exists());
    std::fs::create_dir(&cache.directory).unwrap();
    for bytes in [
        b"not JSON".to_vec(),
        vec![b' '; MAX_BYTES as usize + 1],
        json!({"version":1,"account":"different","windows":{"five_hour":{"used":0.2,"at":NOW}}})
            .to_string()
            .into_bytes(),
    ] {
        std::fs::write(cache.state_path(), bytes).unwrap();
        assert!(cache.usage_at(NOW).is_none());
    }
}
