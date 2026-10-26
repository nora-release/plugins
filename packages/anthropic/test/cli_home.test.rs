use super::*;

#[tokio::test]
async fn custom_claude_config_uses_its_profile_keychain_and_cli_environment() {
    const CHILD: &str = "NORA_TEST_CLAUDE_CONFIG";
    let Some(executable) = std::env::var_os(CHILD).map(PathBuf::from) else {
        let directory = fixture::Directory::new();
        let own = directory.0.join("isolated-home");
        std::fs::create_dir_all(&own).unwrap();
        // The explicit default directory still has custom-directory semantics
        // in Claude Code, including its hashed Keychain service and profile.
        for config in [
            directory.0.join("custom Claude cafe\u{301} 用户 "),
            own.join(".claude"),
        ] {
            fixture::profile(&config);
            std::fs::write(
                own.join(".claude.json"),
                json!({"oauthAccount":{"accountUuid":"wrong-default-person"}}).to_string(),
            )
            .unwrap();
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "cli::tests::home_tests::custom_claude_config_uses_its_profile_keychain_and_cli_environment", "--nocapture"])
                .env(CHILD, fixture::binary())
                .env("HOME", &own)
                .env("CLAUDE_CONFIG_DIR", &config)
                .kill_on_drop(true)
                .output()
                .await
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    };
    let selected = PathBuf::from(std::env::var_os("CLAUDE_CONFIG_DIR").unwrap());
    let home = Home::own().unwrap();
    assert_eq!(
        home.directory(),
        selected,
        "The sign-in must follow CLAUDE_CONFIG_DIR"
    );
    assert!(home.is_own(), "The user owns an external configuration");
    assert_eq!(home.profile(), selected.join(".claude.json"));
    let profile = read(&home.profile()).unwrap();
    assert_eq!(profile["oauthAccount"]["accountUuid"], "account-a");
    use sha2::{Digest, Sha256};
    use unicode_normalization::UnicodeNormalization;
    let normalized = selected.to_string_lossy().nfc().collect::<String>();
    let digest = format!("{:x}", Sha256::digest(normalized.as_bytes()));
    assert_eq!(
        home.keychain_service(),
        format!("Claude Code-credentials-{}", &digest[..8])
    );

    let proxy = Proxy::new(None).await.unwrap();
    let environment = env(&home, &proxy, false);
    assert_eq!(environment["CLAUDE_CONFIG_DIR"], selected.to_string_lossy());
    assert_eq!(
        environment["CLAUDE_SECURESTORAGE_CONFIG_DIR"],
        selected.to_string_lossy()
    );
    // output executes only the fake CLI; identify/credentials would read the
    // system Keychain. Check the profile and service name independently above.
    let result = output(
        &executable,
        &["auth", "status", "--json"],
        &home,
        proxy,
        false,
    )
    .await
    .unwrap();
    assert!(result.status.success());
    let status: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(status["accountUuid"], "account-a");
    let records = std::fs::read_dir(&selected)
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("fixture-process-")
        })
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 1);
    let started = read(&records[0].path()).unwrap();
    assert_eq!(started["config"], json!(selected));
    assert_eq!(started["cwd"], json!(selected.canonicalize().unwrap()));

    let account = Account {
        executable: executable.clone(),
        home,
        identity: "fixture-identity".into(),
        email: "person@example.test".into(),
        organization: "Example".into(),
        plan: "max".into(),
        saved: json!({"extras":{"scopes":["user:inference"]}}),
        session_binding: "fixture-login".into(),
    };
    let saved = account.saved();
    assert_eq!(saved["type"], "claudeCode");
    assert_eq!(saved["extras"]["noraClaudeCLI"]["own"], true);
    assert_eq!(
        saved["extras"]["noraClaudeCLI"]["configDirectory"],
        json!(selected)
    );
    assert_eq!(
        saved["extras"]["noraAccountSource"]["configDirectory"],
        json!(selected)
    );
    assert!(saved.get("access").is_none());
    assert!(saved.get("refresh").is_none());

    let root = selected.with_file_name("nora-private-accounts");
    let mut legacy = saved.clone();
    legacy["extras"]["noraClaudeCLI"]
        .as_object_mut()
        .unwrap()
        .remove("configDirectory");
    legacy["extras"]["noraAccountSource"]
        .as_object_mut()
        .unwrap()
        .remove("configDirectory");
    let (_, upgraded, expected) = materialize(&root, "old", &legacy).unwrap();
    assert_eq!(upgraded, account.home);
    assert_eq!(expected.as_deref(), Some("fixture-identity"));

    // Reopening this saved account must not silently follow the next account's
    // CLI directory or create a Nora-owned copy of the user's credentials.
    std::env::set_var(
        "CLAUDE_CONFIG_DIR",
        selected.with_file_name("other-account"),
    );
    let (reopened_executable, reopened, identity) = materialize(&root, "work", &saved).unwrap();
    assert_eq!(reopened_executable, executable);
    assert_eq!(reopened, account.home);
    assert_eq!(identity.as_deref(), Some("fixture-identity"));
    assert!(!root.exists());
    drop(account);
    assert!(selected.join(".credentials.json").is_file());
    assert!(selected.join(".claude.json").is_file());

    for invalid in [
        Value::Null,
        json!(""),
        json!("relative/claude"),
        json!("/bad\npath"),
        json!(false),
    ] {
        let mut malformed = saved.clone();
        malformed["extras"]["noraClaudeCLI"]["configDirectory"] = invalid;
        assert!(
            materialize(&root, "work", &malformed).is_err(),
            "Invalid configured directories cannot fall back to another sign-in"
        );
    }
}

#[test]
fn unset_empty_or_relative_claude_config_keeps_the_default_sign_in() {
    const CHILD: &str = "NORA_TEST_DEFAULT_CLAUDE_CONFIG";
    if std::env::var_os(CHILD).is_none() {
        let directory = fixture::Directory::new();
        for config in [None, Some(""), Some("relative/claude")] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command.args(["--exact", "cli::tests::home_tests::unset_empty_or_relative_claude_config_keeps_the_default_sign_in", "--nocapture"])
                .env(CHILD, "1").env("HOME", &directory.0).env_remove("CLAUDE_CONFIG_DIR");
            if let Some(config) = config {
                command.env("CLAUDE_CONFIG_DIR", config);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    }
    let user_home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let home = Home::own().unwrap();
    assert_eq!(home, Home::Own(user_home.join(".claude")));
    assert_eq!(home.profile(), user_home.join(".claude.json"));
    assert_eq!(home.keychain_service(), "Claude Code-credentials");
}
