use super::*;
use crate::test_support::Directory;
use serde_json::json;

#[tokio::test]
async fn private_atomic_cache_and_process_locks_never_overwrite_or_remove_an_active_lock() {
    let directory = Directory::new();
    let path = directory.0.join("cache/credits.json");
    write(&path, &json!({"remaining":100})).unwrap();
    assert_eq!(read(&path).unwrap()["remaining"], 100);
    let first = Lock::take(&path, Duration::from_secs(1)).await.unwrap();
    let second = Lock::take(&path, Duration::from_millis(30)).await;
    assert!(second.is_err());
    assert!(first.wanted());
    drop(first);
    let second = Lock::take(&path, Duration::from_secs(1)).await.unwrap();
    assert!(!second.wanted());
    drop(second);
    assert!(path.with_extension("since").is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
#[test]
fn malformed_large_cache_files_and_original_times_are_not_reinterpreted_as_fresh_balances() {
    let directory = Directory::new();
    let path = directory.0.join("bad.json");
    std::fs::write(&path, b"bad").unwrap();
    assert!(read(&path).is_none());
    assert_eq!(timestamp(&json!({"fetchedAt":"bad"}), "fetchedAt"), None);
    assert_eq!(
        timestamp(&json!({"fetchedAt":"2026-10-26T01:00:00Z"}), "fetchedAt"),
        Some(1792976400000)
    );
}
