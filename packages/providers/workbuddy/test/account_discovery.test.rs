use super::*;

struct SourceFile(PathBuf);
impl SourceFile {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("nora-workbuddy-discover-{}.json", random_id())))
    }
    fn write(&self, uid: &str, enterprise: &str) {
        std::fs::write(&self.0, json!({"auth":{"accessToken":"private-local-token","refreshToken":"private-local-refresh","expiresAt":now()+3_600_000},"account":{"uid":uid,"enterpriseId":enterprise,"nickname":"Member"}}).to_string()).unwrap();
    }
}
impl Drop for SourceFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn desktop_discovery_is_automatic_read_only_and_stays_bound_to_the_live_source() {
    let file = SourceFile::new();
    assert_eq!(
        discover_at(Site::China, &file.0).unwrap(),
        json!({"accounts":[]})
    );
    file.write("member-1", "enterprise-a");
    let original = std::fs::read(&file.0).unwrap();
    let result = discover_at(Site::China, &file.0).unwrap();
    let account = &result["accounts"][0];
    assert_eq!(account["sourceID"], "desktop");
    assert_eq!(
        account["credentials"]["accountIdentity"],
        json!({"issuer":"https://copilot.tencent.com","subject":"member-1","scope":"enterprise-a"})
    );
    assert_eq!(
        account["credentials"]["extras"]["noraAccountSource"]["kind"],
        "desktop"
    );
    assert_eq!(account["credentials"]["sourcePath"], json!(file.0));
    assert!(!account.to_string().contains("private-local"));
    assert_eq!(std::fs::read(&file.0).unwrap(), original);
    assert_eq!(
        read_source(Site::China, &account["credentials"]).unwrap()["access"],
        "private-local-token"
    );
}

#[test]
fn a_changed_person_or_billing_account_is_discovered_without_reusing_the_old_source_binding() {
    let file = SourceFile::new();
    file.write("member-1", "enterprise-a");
    let first = discover_at(Site::China, &file.0).unwrap()["accounts"][0]["credentials"].clone();
    file.write("member-2", "enterprise-a");
    let second = discover_at(Site::China, &file.0).unwrap()["accounts"][0]["credentials"].clone();
    assert_ne!(first["accountIdentity"], second["accountIdentity"]);
    assert!(read_source(Site::China, &first).is_err());
    file.write("member-2", "enterprise-b");
    let third = discover_at(Site::China, &file.0).unwrap()["accounts"][0]["credentials"].clone();
    assert_ne!(second["accountIdentity"], third["accountIdentity"]);
    assert!(read_source(Site::China, &second).is_err());
    assert!(read_source(Site::China, &third).is_ok());
}

#[test]
fn corrupted_or_encrypted_desktop_sign_ins_are_failures_instead_of_false_sign_outs() {
    let file = SourceFile::new();
    std::fs::write(&file.0, "invalid-json").unwrap();
    assert!(discover_at(Site::China, &file.0).is_err());
    std::fs::write(
        &file.0,
        json!({"auth":{"accessToken":{"$wbEncrypted":"cipher"}},"account":{"uid":"member"}})
            .to_string(),
    )
    .unwrap();
    assert!(discover_at(Site::China, &file.0).is_err());
    std::fs::write(&file.0, json!({"auth":null,"account":null}).to_string()).unwrap();
    assert_eq!(
        discover_at(Site::China, &file.0).unwrap(),
        json!({"accounts":[]})
    );
}

#[test]
fn browser_and_desktop_accounts_share_one_identity_while_regions_and_enterprises_remain_separate() {
    let file = SourceFile::new();
    file.write("member-1", "enterprise-a");
    let local = discover_at(Site::China, &file.0).unwrap()["accounts"][0]["credentials"].clone();
    let mut owned = json!({"type":"oauth","access":"owned-token","uid":"member-1","enterpriseId":"enterprise-a"});
    assign_identity(Site::China, &mut owned).unwrap();
    assert_eq!(local["accountIdentity"], owned["accountIdentity"]);
    assign_identity(Site::International, &mut owned).unwrap();
    assert_ne!(local["accountIdentity"], owned["accountIdentity"]);
}

#[test]
fn a_personal_desktop_or_browser_account_omits_the_absent_enterprise_scope() {
    for site in [Site::China, Site::International] {
        let file = SourceFile::new();
        file.write("personal-member", "");
        let local = discover_at(site, &file.0).unwrap()["accounts"][0]["credentials"].clone();
        assert_eq!(
            local["accountIdentity"],
            json!({"issuer":site.endpoint(),"subject":"personal-member"})
        );
        assert!(local["accountIdentity"].get("scope").is_none());
        let mut owned = json!({"type":"oauth","uid":"personal-member","access":"owned-token"});
        assign_identity(site, &mut owned).unwrap();
        assert_eq!(local["accountIdentity"], owned["accountIdentity"]);
        assert!(read_source(site, &local).is_ok());
    }
}
