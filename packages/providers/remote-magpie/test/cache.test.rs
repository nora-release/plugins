use super::*;
use crate::test_support::Directory;
use serde_json::json;
fn error(held: bool) -> RemoteError {
    RemoteError {
        status: if held { 403 } else { 502 },
        held,
        message: "Temporarily unavailable".into(),
    }
}
fn reservation(cache: &Cache, cards: bool, now: u64, force: bool) -> u64 {
    match cache.begin(cards, now, force).unwrap() {
        Begin::Read(revision) => revision,
        _ => panic!("A fresh read was expected"),
    }
}
#[test]
fn cache_survives_process_restart_and_obeys_the_thirty_second_read_window() {
    let directory = Directory::new();
    let fingerprint = "a".repeat(64);
    let cache = Cache::new(&directory.0, &fingerprint).unwrap();
    let ticket = reservation(&cache, true, 1000, false);
    cache
        .finish(
            true,
            ticket,
            1000,
            Ok(json!({"data":[{"provider":"codex"}]})),
        )
        .unwrap();
    let reopened = Cache::new(&directory.0, &fingerprint).unwrap();
    assert!(matches!(
        reopened.begin(true, 30_999, false).unwrap(),
        Begin::Cached(_)
    ));
    assert!(matches!(
        reopened.begin(true, 31_000, false).unwrap(),
        Begin::Read(_)
    ));
}
#[test]
fn failed_reads_keep_the_actual_previous_value_and_its_original_date() {
    let directory = Directory::new();
    let cache = Cache::new(&directory.0, &"a".repeat(64)).unwrap();
    let ticket = reservation(&cache, false, 1000, true);
    cache
        .finish(
            false,
            ticket,
            1000,
            Ok(json!({"limited":true,"limit":{"tokens":50}})),
        )
        .unwrap();
    let ticket = reservation(&cache, false, 2000, true);
    let read = cache
        .finish(false, ticket, 2000, Err(error(false)))
        .unwrap();
    assert_eq!(read.read_at, 1000);
    assert_eq!(read.value.unwrap()["limit"]["tokens"], 50);
    assert_eq!(read.error, "Temporarily unavailable");
}
#[test]
fn a_restricted_key_drops_its_old_remote_accounts_and_an_older_read_cannot_restore_them() {
    let directory = Directory::new();
    let cache = Cache::new(&directory.0, &"a".repeat(64)).unwrap();
    let initial = reservation(&cache, true, 1000, true);
    cache
        .finish(
            true,
            initial,
            1000,
            Ok(json!({"data":[{"provider":"private"}]})),
        )
        .unwrap();
    let slow = reservation(&cache, true, 2000, true);
    let latest = reservation(&cache, true, 2001, true);
    cache.finish(true, latest, 2100, Err(error(true))).unwrap();
    let read = cache
        .finish(
            true,
            slow,
            2200,
            Ok(json!({"data":[{"provider":"private"}]})),
        )
        .unwrap();
    assert!(read.held);
    assert!(read.value.is_none());
    let next = reservation(&cache, true, 3000, true);
    let read = cache.finish(true, next, 3001, Err(error(false))).unwrap();
    assert!(read.held);
    assert!(read.value.is_none());
}
#[test]
fn endpoint_and_key_fingerprints_never_share_saved_cards() {
    let directory = Directory::new();
    let a = Cache::new(&directory.0, &"a".repeat(64)).unwrap();
    let b = Cache::new(&directory.0, &"b".repeat(64)).unwrap();
    let ticket = reservation(&a, true, 1000, true);
    a.finish(
        true,
        ticket,
        1000,
        Ok(json!({"data":[{"provider":"private"}]})),
    )
    .unwrap();
    let ticket = reservation(&b, true, 1000, true);
    let read = b.finish(true, ticket, 1000, Err(error(false))).unwrap();
    assert!(read.value.is_none());
}
#[cfg(unix)]
#[test]
fn cached_usage_files_are_private_and_symlinks_are_not_followed() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let directory = Directory::new();
    let cache = Cache::new(&directory.0, &"a".repeat(64)).unwrap();
    reservation(&cache, true, 1000, true);
    assert_eq!(
        std::fs::metadata(&cache.path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    std::fs::remove_file(&cache.path).unwrap();
    let target = directory.0.join("unrelated");
    std::fs::write(&target, b"untouched").unwrap();
    symlink(&target, &cache.path).unwrap();
    assert!(cache.begin(true, 2000, true).is_err());
    assert_eq!(std::fs::read(target).unwrap(), b"untouched");
}
