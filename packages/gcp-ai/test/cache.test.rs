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

#[test]
fn cache_digest_keeps_previous_json_and_tuple_bytes() {
    let a: Value = serde_json::from_str(r#"{"z":3,"a":{"y":2,"x":1}}"#).unwrap();
    let b: Value = serde_json::from_str(r#"{"a":{"x":1,"y":2},"z":3}"#).unwrap();
    assert_eq!(digest(&a), digest(&b));
    assert_eq!(
        digest(&a),
        "f80735c375180d651e7c5350543887a94bbef5ed349bb2ab7209fcc0758f66e4"
    );
    assert_ne!(digest(&json!([a, 2])), digest(&json!([2, b])));
    assert_eq!(
        digest(&("project", Some("account"), None::<String>)),
        format!("{:x}", Sha256::digest(br#"["project","account",null]"#))
    );
}
