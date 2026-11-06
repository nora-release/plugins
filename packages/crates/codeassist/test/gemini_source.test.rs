use super::*;

struct Files(PathBuf);
impl Files {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "nora-gemini-source-{}",
            nora_ai::oauth_pkce::random_hex(12)
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn path(&self) -> PathBuf {
        self.0.join("oauth_creds.json")
    }
    fn write(&self, value: Value) {
        std::fs::write(self.path(), serde_json::to_vec(&value).unwrap()).unwrap();
    }
}
impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn snapshot_reads_geminis_project_and_replaces_only_tokens_with_private_atomic_file() {
    let files = Files::new();
    files.write(json!({"access_token":"old-access","refresh_token":"old-refresh","expiry_date":1,"id_token":"kept-id","other":{"kept":true}}));
    std::fs::write(
        files.0.join("google_accounts.json"),
        br#"{"active":"person@example.test"}"#,
    )
    .unwrap();
    std::fs::write(
        files.0.join(".env"),
        "# GOOGLE_CLOUD_PROJECT=wrong\nexport GOOGLE_CLOUD_PROJECT='project-a' # note\n",
    )
    .unwrap();
    let snapshot = Snapshot::read(&files.path()).unwrap();
    assert_eq!(snapshot.tokens.email, "person@example.test");
    assert_eq!(snapshot.tokens.project, "project-a");
    let mut tokens = snapshot.tokens.clone();
    tokens.access = "new-access".into();
    tokens.refresh = "new-refresh".into();
    tokens.expires = now() + 3600000;
    snapshot.write_renewed(&tokens).unwrap();
    let value = json_file(&files.path()).unwrap();
    assert_eq!(value["access_token"], "new-access");
    assert_eq!(value["refresh_token"], "new-refresh");
    assert_eq!(value["id_token"], "kept-id");
    assert_eq!(value["other"]["kept"], true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(files.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert_eq!(std::fs::read_dir(&files.0).unwrap().count(), 3);
}

#[test]
fn changed_or_removed_source_is_never_overwritten_and_cache_epoch_changes() {
    let files = Files::new();
    files.write(json!({"refresh_token":"original","access_token":"old"}));
    let snapshot = Snapshot::read(&files.path()).unwrap();
    files.write(json!({"refresh_token":"new-login","access_token":"new"}));
    let current = std::fs::read(files.path()).unwrap();
    assert_ne!(Snapshot::read(&files.path()).unwrap().key, snapshot.key);
    assert!(snapshot.write_renewed(&snapshot.tokens).is_err());
    assert_eq!(std::fs::read(files.path()).unwrap(), current);
    std::fs::remove_file(files.path()).unwrap();
    assert!(snapshot.write_renewed(&snapshot.tokens).is_err());
    assert!(!files.path().exists());
}
#[cfg(unix)]
#[test]
fn symbolic_source_keeps_its_link_and_repointing_or_replacing_the_source_invalidates_the_snapshot()
{
    use std::os::unix::fs::symlink;
    let files = Files::new();
    files.write(json!({"refresh_token":"old-refresh","access_token":"old-access"}));
    let link = files.0.join("linked.json");
    symlink(files.path(), &link).unwrap();
    let snapshot = Snapshot::read(&link).unwrap();
    let mut tokens = snapshot.tokens.clone();
    tokens.access = "new-access".into();
    snapshot.write_renewed(&tokens).unwrap();
    assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
    assert_eq!(
        json_file(&files.path()).unwrap()["access_token"],
        "new-access"
    );
    let snapshot = Snapshot::read(&link).unwrap();
    let replacement = files.0.join("replacement.json");
    std::fs::write(&replacement, std::fs::read(files.path()).unwrap()).unwrap();
    std::fs::rename(replacement, files.path()).unwrap();
    assert!(snapshot.ensure_current().is_err());
    let snapshot = Snapshot::read(&link).unwrap();
    std::fs::remove_file(&link).unwrap();
    let other = files.0.join("other.json");
    std::fs::write(&other, std::fs::read(files.path()).unwrap()).unwrap();
    symlink(other, &link).unwrap();
    assert!(snapshot.write_renewed(&tokens).is_err());
}

#[test]
fn references_keep_verified_identity_and_no_access_or_refresh_tokens() {
    let tokens = session::Tokens {
        uid: "account-a".into(),
        email: "person@example.test".into(),
        access: "private-access".into(),
        refresh: "private-refresh".into(),
        identity: Some(json!({"issuer":"https://accounts.google.com","subject":"123"})),
        ..Default::default()
    };
    let value = saved(&tokens, Path::new("/tmp/.gemini/oauth_creds.json")).unwrap();
    assert_eq!(value["source"]["uid"], "account-a");
    assert_eq!(value["authMethod"], METHOD);
    assert_eq!(value["extras"]["noraAccountSource"]["kind"], METHOD);
    assert_eq!(value["accountIdentity"]["subject"], "123");
    assert!(!value.to_string().contains("private-"));
    assert!(bound(&value));
}

#[test]
fn dotenv_projects_preserve_quoted_values_and_ignore_comments() {
    assert_eq!(
        project_from_env("#GOOGLE_CLOUD_PROJECT=no\nGOOGLE_CLOUD_PROJECT=one #comment\n"),
        Some("one".into())
    );
    assert_eq!(
        project_from_env("GOOGLE_CLOUD_PROJECT=\"two\"\n"),
        Some("two".into())
    );
    assert_eq!(project_from_env("GOOGLE_CLOUD_PROJECT=\n"), None);
}
