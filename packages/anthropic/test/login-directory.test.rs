use super::*;

fn fixture() -> PathBuf {
    let path = std::env::temp_dir().join(format!("nora-private-login-cleanup-{}", cli::random()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn only_an_unclaimed_sign_in_directory_is_removed() {
    let path = fixture();
    let pending = Directory::new(path.clone());
    pending.clear_with(None, Duration::ZERO);
    assert!(!path.exists());
    // Repeated cancellation cannot remove a different owner of the path.
    std::fs::create_dir_all(&path).unwrap();
    pending.clear_with(None, Duration::ZERO);
    drop(pending);
    assert!(path.exists());
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn completed_accounts_keep_their_profile_and_credentials() {
    let path = fixture();
    let pending = Directory::new(path.clone());
    pending.keep();
    pending.clear_with(None, Duration::ZERO);
    drop(pending);
    assert!(path.exists());
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn unavailable_keychain_command_does_not_keep_the_profile() {
    let path = fixture();
    let pending = Directory::new(path.clone());
    pending.clear_with(
        Some(&path.join("missing-command")),
        Duration::from_millis(50),
    );
    assert!(!path.exists());
}
