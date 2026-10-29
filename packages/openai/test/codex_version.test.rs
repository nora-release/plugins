use super::*;
#[test]
fn floor_cache_and_installed_cli_are_compared_numerically() {
    assert_eq!(
        newest([Some(MINIMUM), Some("0.160.0"), Some("codex-cli 0.161.1")]),
        "0.161.1"
    );
    assert_eq!(newest([Some(MINIMUM), Some("0.160.2"), None]), "0.160.2");
    assert_eq!(
        newest([Some(MINIMUM), Some("0.99.20"), Some("not a version")]),
        MINIMUM
    );
    assert_eq!(newest([Some("0.9.99"), Some("0.10.0")]), "0.10.0");
    assert_eq!(newest([Some("0.160.9"), Some("0.160.10")]), "0.160.10");
}
#[test]
fn version_output_and_prerelease_suffixes_match_magpie_version_parsing() {
    assert_eq!(
        normalized("codex-cli 0.160.0-alpha.3"),
        Some("0.160.0".into())
    );
    assert_eq!(
        normalized("\u{1b}[32mcodex-cli 0.161.2\u{1b}[0m"),
        Some("0.161.2".into())
    );
    for value in ["", "0.159", "vlatest", "1.two.3"] {
        assert_eq!(normalized(value), None);
    }
}
#[test]
fn project_or_temporary_codex_programs_are_not_executed() {
    for path in [
        "/tmp/codex",
        "/some/project/node_modules/.bin/codex",
        "./codex",
        "/usr/local/bin/codex-other",
    ] {
        assert!(!trusted_native_path(Path::new(path)));
    }
    assert!(trusted_native_path(Path::new(
        "/Applications/Codex.app/Contents/Resources/codex"
    )));
    assert!(trusted_native_path(Path::new(
        "/opt/homebrew/Cellar/codex/0.160.0/bin/codex"
    )));
}

#[test]
fn installed_npm_package_and_cli_cache_are_read_without_running_programs() {
    let root = std::env::temp_dir().join(format!("nora-codex-version-{}", std::process::id()));
    let package = root.join("node_modules/@openai/codex");
    std::fs::create_dir_all(package.join("bin")).unwrap();
    std::fs::write(
        package.join("package.json"),
        br#"{"name":"@openai/codex","version":"0.161.1"}"#,
    )
    .unwrap();
    std::fs::write(package.join("bin/codex.js"), b"This fixture must never run").unwrap();
    let cache = root.join("models_cache.json");
    std::fs::write(&cache, br#"{"client_version":"0.160.2","models":[]}"#).unwrap();
    let cached = cache_version(&cache).unwrap();
    let installed = npm_version(&package.join("bin/codex.js")).unwrap();
    assert_eq!(
        newest([Some(MINIMUM), Some(&cached), Some(&installed)]),
        "0.161.1"
    );
    std::fs::write(&cache, b"malformed").unwrap();
    assert!(cache_version(&cache).is_none());
    std::fs::remove_dir_all(root).unwrap();
}
