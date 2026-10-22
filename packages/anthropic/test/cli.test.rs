use super::*;
use crate::fixture;
#[tokio::test]
async fn real_executable_status_is_scoped_to_the_private_profile_and_identity() {
    let directory = fixture::Directory::new();
    let root = directory.0.join("claude-accounts");
    let path = root.join("a");
    let value = fixture::profile(&path);
    let proxy = Proxy::new(None).await.unwrap();
    let account = identify(
        &fixture::binary(),
        &path,
        proxy.clone(),
        value["accountId"].as_str(),
    )
    .await
    .unwrap();
    assert_eq!(account.email, "person@example.test");
    assert_eq!(account.organization, "Example");
    assert!(
        identify(&fixture::binary(), &path, proxy, Some("different-account"))
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
    let env = env(&directory.0, &proxy, false);
    assert_eq!(env["CLAUDE_CONFIG_DIR"], directory.0.to_string_lossy());
    assert_eq!(env["ENABLE_CLAUDEAI_MCP_SERVERS"], "0");
    assert_eq!(env["CLAUDE_CODE_DISABLE_AUTO_MEMORY"], "1");
    assert_eq!(env["NO_PROXY"], "*");
    assert!(!env.contains_key("ANTHROPIC_API_KEY"));
    assert!(!env.contains_key("ANTHROPIC_BASE_URL"));
    assert_eq!(env["CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"], "1");
    assert!(!super::env(&directory.0, &proxy, true)
        .contains_key("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"));
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
