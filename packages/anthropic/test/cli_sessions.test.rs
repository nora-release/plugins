use super::*;

fn account(root: &Path) -> cli::Account {
    cli::Account {
        executable: root.join("claude"),
        home: cli::Home::Isolated(root.to_owned()),
        identity: "account-a".into(),
        email: "person@example.test".into(),
        organization: "org-a".into(),
        plan: "max".into(),
        saved: serde_json::json!({}),
        session_binding: "login-a".into(),
    }
}

#[test]
fn private_work_directory_is_stable_for_one_account_and_separate_for_other_accounts() {
    let root = crate::fixture::Directory::new();
    let first = account(&root.0);
    let work = work_directory(&first, &root.0).unwrap();
    assert_eq!(work, work_directory(&first, &root.0).unwrap());
    let mut second = first.clone();
    second.identity = "account-b".into();
    let other = work_directory(&second, &root.0).unwrap();
    assert_ne!(work, other);
    assert!(!work.starts_with(&root.0));
    assert!(trusted(&work, true));
    std::fs::remove_dir(work).unwrap();
    std::fs::remove_dir(other).unwrap();
}

#[test]
fn trusted_reported_session_files_follow_cleanup_ownership() {
    let root = crate::fixture::Directory::new();
    let account = account(&root.0);
    let work = Path::new("/private/tmp/work-account-a");
    for id in [
        "../another",
        "another/file",
        "another\\file",
        "",
        "session.jsonl",
    ] {
        assert!(Session::capture(&account, work, id).is_err());
    }
    let session = Session::capture(&account, work, "session-a_1").unwrap();
    assert!(!session.exists());
    cli::directory(&session.project).unwrap();
    let path = session.project.join("session-a_1.jsonl");
    cli::write(&path, b"one conversation").unwrap();
    assert!(session.exists());
    drop(session);
    assert!(!path.exists());
}

#[test]
#[cfg(unix)]
fn session_files_outside_the_private_project_cannot_be_followed_or_deleted() {
    let root = crate::fixture::Directory::new();
    let account = account(&root.0);
    let session = Session::capture(&account, Path::new("/work"), "session-a").unwrap();
    cli::directory(&root.0.join("projects")).unwrap();
    let external = root.0.join("external");
    cli::directory(&external).unwrap();
    cli::write(&external.join("session-a.jsonl"), b"unrelated").unwrap();
    std::os::unix::fs::symlink(&external, &session.project).unwrap();
    assert!(!session.exists());
    drop(session);
    assert!(external.join("session-a.jsonl").exists());
}
