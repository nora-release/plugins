use super::*;
use crate::fixture;
#[tokio::test]
async fn real_executable_status_is_scoped_to_the_private_profile_and_identity() {
    let directory = fixture::Directory::new();
    let root = directory.0.join("claude-accounts");
    let path = root.join("a");
    let value = fixture::profile(&path);
    let proxy = Proxy::new(None).await.unwrap();
    let home = Home::Isolated(path.clone());
    let account = identify(
        &fixture::binary(),
        &home,
        proxy.clone(),
        value["accountId"].as_str(),
    )
    .await
    .unwrap();
    assert_eq!(account.email, "person@example.test");
    assert_eq!(account.organization, "Example");
    assert!(
        identify(&fixture::binary(), &home, proxy, Some("different-account"))
            .await
            .is_err()
    );
    let profile = account.saved();
    assert_eq!(profile["type"], "oauth");
    assert!(profile["extras"]["noraClaudeCLI"]["directory"]
        .as_str()
        .unwrap()
        .starts_with(root.to_str().unwrap()));
}
#[test]
fn private_account_paths_cannot_escape_the_configured_root() {
    let directory = fixture::Directory::new();
    let root = directory.0.join("accounts");
    let path = root.join("a");
    let mut value = fixture::profile(&path);
    assert!(materialize(&root, "work", &value).is_ok());
    value["extras"]["noraClaudeCLI"]["directory"] = json!(root.join("../outside"));
    std::fs::create_dir_all(directory.0.join("outside")).unwrap();
    assert!(materialize(&root, "work", &value).is_err());
}
#[tokio::test]
async fn env_removes_foreign_routing_and_disables_global_tools_without_exposing_proxy_passwords() {
    let directory = fixture::Directory::new();
    let proxy = Proxy::new(None).await.unwrap();
    let home = Home::Isolated(directory.0.clone());
    let env = env(&home, &proxy, false);
    assert_eq!(env["CLAUDE_CONFIG_DIR"], directory.0.to_string_lossy());
    assert_eq!(env["ENABLE_CLAUDEAI_MCP_SERVERS"], "0");
    assert_eq!(env["CLAUDE_CODE_DISABLE_AUTO_MEMORY"], "1");
    assert_eq!(env["NO_PROXY"], "*");
    assert!(!env.contains_key("ANTHROPIC_API_KEY"));
    assert!(!env.contains_key("ANTHROPIC_BASE_URL"));
    assert_eq!(env["CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"], "1");
    assert!(
        !super::env(&home, &proxy, true).contains_key("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC")
    );
}
#[tokio::test]
async fn claude_code_own_sign_in_runs_without_a_configuration_directory_of_nora() {
    let proxy = Proxy::new(None).await.unwrap();
    let env = env(
        &Home::Own(PathBuf::from("/Users/person/.claude")),
        &proxy,
        false,
    );
    assert!(!env.contains_key("CLAUDE_CONFIG_DIR"));
    assert!(!env.contains_key("CLAUDE_SECURESTORAGE_CONFIG_DIR"));
    assert_eq!(env["ENABLE_CLAUDEAI_MCP_SERVERS"], "0");
}
#[test]
fn claude_code_keeps_its_own_sign_in_under_its_plain_names() {
    let own = Home::Own(PathBuf::from("/Users/person/.claude"));
    assert_eq!(own.keychain_service(), "Claude Code-credentials");
    assert_eq!(own.profile(), PathBuf::from("/Users/person/.claude.json"));
    let isolated = Home::Isolated(PathBuf::from("/data/claude-accounts/account-1"));
    assert!(isolated
        .keychain_service()
        .starts_with("Claude Code-credentials-"));
    assert_eq!(
        isolated.profile(),
        PathBuf::from("/data/claude-accounts/account-1/.claude.json")
    );
}
#[test]
fn an_own_sign_in_account_is_saved_without_tokens_and_found_again_by_identity() {
    let directory = fixture::Directory::new();
    let executable = directory.0.join("claude");
    std::fs::write(&executable, "").unwrap();
    let account = Account {
        executable: executable.clone(),
        home: Home::Own(directory.0.join(".claude")),
        identity: "identity-1".into(),
        email: "person@example.com".into(),
        organization: String::new(),
        plan: "max".into(),
        saved: json!({"extras":{"scopes":["user:inference"]}}),
    };
    let saved = account.saved();
    assert!(saved.get("access").is_none());
    assert!(saved.get("refresh").is_none());
    assert_eq!(saved["extras"]["noraClaudeCLI"]["own"], true);
    assert!(saved["extras"]["noraClaudeCLI"].get("directory").is_none());

    let (found, home, expected) =
        materialize(&directory.0.join("accounts"), "work", &saved).unwrap();
    assert_eq!(found, executable);
    assert!(home.is_own());
    assert_eq!(expected.as_deref(), Some("identity-1"));
}
#[test]
fn private_files_are_written_atomically_with_owner_only_permissions() {
    let directory = fixture::Directory::new();
    let path = directory.0.join("credentials");
    write(&path, b"one").unwrap();
    write(&path, b"two").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"two");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
}

#[tokio::test]
async fn identity_uses_the_verified_account_and_organization_instead_of_the_display_email() {
    let directory = fixture::Directory::new();
    let path = directory.0.join("profile");
    fixture::profile(&path);
    let proxy = Proxy::new(None).await.unwrap();
    let first = identify(
        &fixture::binary(),
        &Home::Isolated(path.clone()),
        proxy.clone(),
        None,
    )
    .await
    .unwrap()
    .saved();
    assert_eq!(
        first["accountIdentity"],
        json!({"issuer":"https://claude.ai","subject":"account-a","scope":"org-a"})
    );
    assert_eq!(first["extras"]["accountIdentity"], first["accountIdentity"]);
    let profile = path.join(".claude.json");
    let mut value: Value = serde_json::from_slice(&std::fs::read(&profile).unwrap()).unwrap();
    value["oauthAccount"]["organizationUuid"] = json!("org-b");
    std::fs::write(&profile, value.to_string()).unwrap();
    let second = identify(&fixture::binary(), &Home::Isolated(path), proxy, None)
        .await
        .unwrap()
        .saved();
    assert_eq!(first["email"], second["email"]);
    assert_ne!(first["accountIdentity"], second["accountIdentity"]);
}

#[cfg(unix)]
#[test]
fn installed_local_cli_is_discoverable_without_requiring_the_desktop_apps_path_to_include_it() {
    use std::os::unix::fs::PermissionsExt;
    let directory = fixture::Directory::new();
    let executable = directory.0.join(".local/bin/claude");
    std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
    std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        installed_executable(Some(&directory.0), std::ffi::OsStr::new("/usr/bin:/bin")),
        Some(executable)
    );
}

#[tokio::test]
async fn an_account_without_an_organization_omits_scope_instead_of_returning_an_invalid_empty_scope(
) {
    let directory = fixture::Directory::new();
    let path = directory.0.join("profile");
    fixture::profile(&path);
    let profile = path.join(".claude.json");
    let mut value: Value = serde_json::from_slice(&std::fs::read(&profile).unwrap()).unwrap();
    value["oauthAccount"]
        .as_object_mut()
        .unwrap()
        .remove("organizationUuid");
    std::fs::write(&profile, value.to_string()).unwrap();
    let account = signed_in(
        &fixture::binary(),
        &Home::Isolated(path),
        Proxy::new(None).await.unwrap(),
        None,
    )
    .await
    .unwrap()
    .unwrap()
    .saved();
    assert_eq!(
        account["accountIdentity"],
        json!({"issuer":"https://claude.ai","subject":"account-a"})
    );
    assert!(account["accountIdentity"].get("scope").is_none());
    assert_eq!(
        account["extras"]["accountIdentity"],
        account["accountIdentity"]
    );
}
