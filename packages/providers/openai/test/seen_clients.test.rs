use super::*;

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("nora-codex-seen-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn seen(&self) -> SeenClients {
        let mut seen = SeenClients::default();
        seen.configure(&self.0);
        seen
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn codex_cli_desktop_originator_and_header_precedence_match_official_detection() {
    for (user_agent, version, originator, expected) in [
        (Some("Claude-Code/2.1.310"), None, None, None),
        (Some("Mozilla/5.0"), Some("9.9.9"), None, None),
        (Some("codex_cli_rs/0.160.0"), None, None, Some("0.160.0")),
        (Some("CODEX/0.160.1"), None, None, Some("0.160.1")),
        (
            Some("Codex Desktop/0.162.3 (Mac OS X)"),
            None,
            Some("codex_desktop"),
            Some("0.162.3"),
        ),
        (
            Some("Some Client/0.162.4"),
            None,
            Some("codex_desktop"),
            Some("0.162.4"),
        ),
        (
            Some("Codex/0.160.0"),
            Some("codex-cli 0.163.0-alpha.1"),
            None,
            Some("0.163.0"),
        ),
        (
            Some("codex_cli_rs/0.160.0"),
            Some("not a version"),
            None,
            Some("0.160.0"),
        ),
        (Some("codex/version 0.160.0"), None, None, None),
        (Some("codex/123 wrong 0.160.0"), None, None, None),
        (Some("codex/0..160.0"), None, None, None),
        (Some("codex/0.160"), None, None, None),
        (None, Some("0.163.0"), Some("codex_cli_rs"), Some("0.163.0")),
        (None, Some("0.163.0"), None, None),
        (None, None, Some("codex_cli_rs"), None),
    ] {
        assert_eq!(
            client_version(user_agent, version, originator).as_deref(),
            expected,
            "{user_agent:?} {version:?} {originator:?}"
        );
    }
}

#[test]
fn only_the_highest_version_is_remembered_and_metadata_workers_pick_up_changes() {
    let directory = Directory::new();
    let mut metadata = directory.seen();
    let mut fetch = directory.seen();
    assert_eq!(metadata.get(), None);
    fetch.observe(Some("codex_cli_rs/0.160.0"), None, None);
    assert_eq!(metadata.get().as_deref(), Some("0.160.0"));
    fetch.observe(Some("Codex Desktop/0.162.3"), None, Some("codex_desktop"));
    fetch.observe(Some("codex_cli_rs/0.150.0"), None, None);
    assert_eq!(metadata.get().as_deref(), Some("0.162.3"));
    metadata.observe(Some("Mozilla/5.0"), Some("9.9.9"), None);
    assert_eq!(metadata.get().as_deref(), Some("0.162.3"));
    assert_eq!(
        std::fs::read_to_string(directory.0.join(VERSION_FILE)).unwrap(),
        "0.162.3"
    );
    assert_eq!(directory.seen().get().as_deref(), Some("0.162.3"));
    let other = Directory::new();
    metadata.configure(&other.0);
    assert_eq!(metadata.get(), None);
}

#[test]
fn concurrent_workers_cannot_replace_a_newer_version_with_an_older_one() {
    let directory = Directory::new();
    let workers = (160..176)
        .map(|minor| {
            let mut seen = directory.seen();
            std::thread::spawn(move || {
                seen.observe(Some(&format!("codex_cli_rs/0.{minor}.0")), None, None)
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(directory.seen().get().as_deref(), Some("0.175.0"));
    assert!(std::fs::read_dir(&directory.0).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with('.')));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(directory.0.join(VERSION_FILE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn malformed_or_oversized_saved_versions_and_unwritable_stores_do_not_break_requests() {
    let directory = Directory::new();
    for bytes in [
        b"invalid".to_vec(),
        vec![b'9'; MAX_VERSION_BYTES as usize + 1],
    ] {
        std::fs::write(directory.0.join(VERSION_FILE), bytes).unwrap();
        let mut seen = directory.seen();
        assert_eq!(seen.get(), None);
        seen.observe(Some("codex/0.162.3"), None, None);
        assert_eq!(seen.get().as_deref(), Some("0.162.3"));
    }
    let file = directory.0.join("file");
    std::fs::write(&file, b"cannot be a directory").unwrap();
    let mut seen = SeenClients::default();
    seen.configure(&file);
    seen.observe(Some("codex/0.163.0"), None, None);
    assert_eq!(seen.get().as_deref(), Some("0.163.0"));
}

#[test]
fn compiled_workers_share_observed_versions_without_a_shared_memory_cache() {
    let directory = Directory::new();
    for (version, expected) in [
        ("0.160.0", "0.160.0"),
        ("0.162.3", "0.162.3"),
        ("0.150.0", "0.162.3"),
    ] {
        let worker_name = format!(
            "{}::cross_process_worker",
            module_path!().split_once("::").unwrap().1
        );
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &worker_name, "--ignored", "--nocapture"])
            .env("NORA_CODEX_TEST_DIRECTORY", &directory.0)
            .env("NORA_CODEX_TEST_VERSION", version)
            .env("NORA_CODEX_TEST_EXPECTED", expected)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("running 1 test") && stdout.contains("1 passed; 0 failed"),
            "{stdout}"
        );
        assert_eq!(directory.seen().get().as_deref(), Some(expected));
    }
}

#[test]
#[ignore = "launched by compiled_workers_share_observed_versions_without_a_shared_memory_cache"]
fn cross_process_worker() {
    let directory = PathBuf::from(std::env::var_os("NORA_CODEX_TEST_DIRECTORY").unwrap());
    let version = std::env::var("NORA_CODEX_TEST_VERSION").unwrap();
    let expected = std::env::var("NORA_CODEX_TEST_EXPECTED").unwrap();
    let mut fetch = SeenClients::default();
    fetch.configure(&directory);
    fetch.observe(Some(&format!("codex_cli_rs/{version}")), None, None);
    let mut metadata = SeenClients::default();
    metadata.configure(&directory);
    assert_eq!(metadata.get().as_deref(), Some(expected.as_str()));
}
