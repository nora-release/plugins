use super::*;

#[test]
fn minimum_version_matches_magpie_without_reducing_newer_versions() {
    assert_eq!(MINIMUM, "0.159.0");
    assert_eq!(newest([Some(MINIMUM), Some("0.158.9")]), "0.159.0");
}

#[test]
fn codex_user_agents_match_magpies_platform_names_and_terminals() {
    for (platform, architecture, release, expected) in [
        (
            "macos",
            "aarch64",
            "26.6\n",
            "codex_cli_rs/0.170.2 (Mac OS 26.6.0; arm64) Apple_Terminal/455",
        ),
        (
            "macos",
            "x86_64",
            "15.7.1\n",
            "codex_cli_rs/0.170.2 (Mac OS 15.7.1; x86_64) Apple_Terminal/455",
        ),
        (
            "windows",
            "x86_64",
            "Microsoft Windows [Version 10.0.26100.4652]",
            "codex_cli_rs/0.170.2 (Windows 10.0.26100; x86_64) WindowsTerminal",
        ),
        (
            "linux",
            "x86_64",
            "6.8.0-63-generic\n",
            "codex_cli_rs/0.170.2 (Linux 6.8.0; x86_64) xterm-256color",
        ),
    ] {
        assert_eq!(
            user_agent_for(
                "0.170.2",
                platform,
                architecture,
                &system_name(platform, Some(release))
            ),
            expected
        );
    }
    for (platform, expected) in [
        ("macos", "Mac OS"),
        ("windows", "Windows"),
        ("linux", "Linux"),
    ] {
        assert_eq!(system_name(platform, None), expected);
        assert_eq!(system_name(platform, Some("unavailable")), expected);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn codex_identity_headers_use_the_hosts_macos_version_and_architecture() {
    let output = std::process::Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .unwrap();
    assert!(output.status.success());
    let mut release = String::from_utf8(output.stdout).unwrap().trim().to_owned();
    while release.matches('.').count() < 2 {
        release.push_str(".0");
    }
    let architecture = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x86_64"
    };
    let headers = headers("0.170.2");
    assert_eq!(headers["originator"], "codex_cli_rs");
    assert_eq!(headers["version"], "0.170.2");
    assert_eq!(
        headers["user-agent"],
        format!("codex_cli_rs/0.170.2 (Mac OS {release}; {architecture}) Apple_Terminal/455")
    );
}

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
