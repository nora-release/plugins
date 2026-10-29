use super::*;
use serde_json::json;

struct Fixture(PathBuf);
impl Fixture {
    fn new(value: Value) -> Self {
        let path = std::env::temp_dir().join(format!(
            "nora-kiro-magpie-test-{}.json",
            nora_ai::oauth_pkce::random_hex(12)
        ));
        std::fs::write(&path, value.to_string()).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn oauth() -> Value {
    json!({"type":"oauth","method":"social","access":"magpie-private-access","refresh":"magpie-private-refresh","expires":1_900_000_000_000_i64,"profileArn":"arn:aws:codewhisperer:us-east-1:123456789012:profile/test","accountId":"person@example.test (Google)","uid":"google","loginProvider":"Google","plan":"Pro"})
}

#[test]
fn default_configuration_path_matches_magpie_absolute_xdg_rules() {
    let home = std::env::temp_dir().join("fixture-home");
    let mut env = BTreeMap::new();
    assert_eq!(
        default_path_at(&home, &env),
        home.join(".config/magpie/plugin-auth.json")
    );
    env.insert("XDG_CONFIG_HOME".into(), "relative-config".into());
    assert_eq!(
        default_path_at(&home, &env),
        home.join(".config/magpie/plugin-auth.json")
    );
    let config = std::env::temp_dir().join("fixture-xdg-config");
    env.insert("XDG_CONFIG_HOME".into(), config.to_string_lossy().into());
    assert_eq!(
        default_path_at(&home, &env),
        config.join("magpie/plugin-auth.json")
    );
}

#[test]
fn all_owned_kiro_oauth_slots_are_discovered_without_following_source_markers_or_other_providers() {
    let fixture = Fixture::new(
        json!({"kiro#z":oauth(),"kiro":oauth(),"kiro#a":oauth(),"kiro#marker":{"type":"oauth","source":"kiro-cli","access":"","refresh":""},"kiro#api":{"type":"api","key":"other-key"},"kiro#bad/key":oauth(),"openai":oauth()}),
    );
    assert_eq!(
        list_sources(&fixture.0)
            .unwrap()
            .iter()
            .map(MagpieAccount::as_str)
            .collect::<Vec<_>>(),
        vec!["kiro", "kiro#a", "kiro#z"]
    );
    assert!(
        read_credential(&MagpieAccount::new("kiro#marker").unwrap(), &fixture.0)
            .unwrap()
            .is_none()
    );
    assert!(
        read_credential(&MagpieAccount::new("kiro#api").unwrap(), &fixture.0)
            .unwrap()
            .is_none()
    );
    assert!(
        read_credential(&MagpieAccount::new("kiro#missing").unwrap(), &fixture.0)
            .unwrap()
            .is_none()
    );
}

#[test]
fn display_account_id_and_login_method_uid_are_not_promoted_to_verified_user_identity() {
    let original = json!({"kiro#slot":oauth()});
    let fixture = Fixture::new(original.clone());
    let credential = read_credential(&MagpieAccount::new("kiro#slot").unwrap(), &fixture.0)
        .unwrap()
        .unwrap();
    assert_eq!(credential.access, "magpie-private-access");
    assert_eq!(credential.refresh, "magpie-private-refresh");
    assert_eq!(credential.expires, 1_900_000_000_000_i64);
    assert_eq!(credential.method, "social");
    assert!(credential.identity.is_empty());
    assert!(credential.email.is_empty());
    assert!(credential.user_id.is_empty());
    assert!(credential.account_identity().is_none());
    assert_eq!(read_document(&fixture.0).unwrap(), Some(original));
}

#[test]
fn an_owned_builder_id_login_keeps_its_renewal_client_and_builder_mapping() {
    let mut value = oauth();
    value["method"] = json!("idc");
    value["loginProvider"] = json!("BuilderId");
    value["clientId"] = json!("client");
    value["clientSecret"] = json!("private-client-secret");
    let fixture = Fixture::new(json!({"kiro":value}));
    let credential = read_credential(&MagpieAccount::new("kiro").unwrap(), &fixture.0)
        .unwrap()
        .unwrap();
    assert!(credential.builder);
    assert_eq!(credential.method, "idc");
    assert_eq!(credential.client_id, "client");
    assert_eq!(credential.client_secret, "private-client-secret");
    assert!(credential.user_id.is_empty());
}

#[test]
fn unreadable_magpie_documents_are_errors_and_a_missing_file_is_not_a_signout_of_other_sources() {
    let fixture = Fixture::new(json!({}));
    std::fs::remove_file(&fixture.0).unwrap();
    assert!(list_sources(&fixture.0).unwrap().is_empty());
    assert!(
        read_credential(&MagpieAccount::new("kiro").unwrap(), &fixture.0)
            .unwrap()
            .is_none()
    );
    for content in ["broken-private-json", "[]", "null"] {
        std::fs::write(&fixture.0, content).unwrap();
        let error = list_sources(&fixture.0).err().unwrap();
        assert!(!error.message.contains("private"));
    }
    std::fs::write(
        &fixture.0,
        json!({"kiro":{"type":"oauth","access":true}}).to_string(),
    )
    .unwrap();
    assert_eq!(
        list_sources(&fixture.0).unwrap(),
        vec![MagpieAccount::new("kiro").unwrap()]
    );
    assert!(read_credential(&MagpieAccount::new("kiro").unwrap(), &fixture.0).is_err());
}

#[test]
fn official_magpie_write_lock_is_released_without_removing_a_successor_lock() {
    let fixture = Fixture::new(json!({}));
    let path = PathBuf::from(format!("{}.lock", fixture.0.to_string_lossy()));
    let lock = WriteLock::acquire(&fixture.0).unwrap();
    assert!(lock.still_owned());
    drop(lock);
    assert!(!path.exists());
    let lock = WriteLock::acquire(&fixture.0).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, "successor-lock").unwrap();
    assert!(!lock.still_owned());
    drop(lock);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "successor-lock");
    std::fs::remove_file(path).unwrap();
}
