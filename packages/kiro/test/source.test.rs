use super::*;

const PROFILE: &str = "arn:aws:codewhisperer:us-east-1:123456789012:profile/test";

struct Fixture {
    directory: PathBuf,
    path: PathBuf,
    locks: PathBuf,
}
impl Fixture {
    fn new(kind: &str, value: Value) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "nora-kiro-source-test-{}",
            nora_ai::oauth_pkce::random_hex(12)
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join(if kind == "cli" {
            "data.sqlite3"
        } else {
            "kiro-auth-token.json"
        });
        if kind == "cli" {
            let db = rusqlite::Connection::open(&path).unwrap();
            db.execute_batch("CREATE TABLE auth_kv (key TEXT PRIMARY KEY,value TEXT NOT NULL)")
                .unwrap();
            db.execute(
                "INSERT INTO auth_kv VALUES (?1,?2)",
                ["kirocli:social:token", &value.to_string()],
            )
            .unwrap();
        } else {
            std::fs::write(&path, value.to_string()).unwrap();
        }
        let locks = directory.join("locks");
        Self {
            directory,
            path,
            locks,
        }
    }
    fn cli() -> Self {
        Self::new(
            "cli",
            json!({"access_token":"old-access","refresh_token":"old-refresh","expires_at":"2020-01-01T00:00:00Z","profile_arn":PROFILE,"region":"us-east-1","unknown":{"keep":true}}),
        )
    }
    fn ide() -> Self {
        Self::new(
            "ide",
            json!({"accessToken":"old-access","refreshToken":"old-refresh","expiresAt":"2020-01-01T00:00:00Z","profileArn":PROFILE,"region":"us-east-1","authMethod":"social","unknown":{"keep":true}}),
        )
    }
    fn open(&self, kind: &SourceKind) -> SourceSession {
        SourceSession::open(kind, self.path.to_str().unwrap(), &self.locks)
            .unwrap()
            .unwrap()
    }
    fn row(&self, key: &str) -> String {
        rusqlite::Connection::open(&self.path)
            .unwrap()
            .query_row("SELECT value FROM auth_kv WHERE key=?1", [key], |row| {
                row.get(0)
            })
            .unwrap()
    }
    fn write_row(&self, key: &str, value: &Value) {
        rusqlite::Connection::open(&self.path)
            .unwrap()
            .execute(
                "INSERT OR REPLACE INTO auth_kv VALUES (?1,?2)",
                [key, &value.to_string()],
            )
            .unwrap();
    }
    fn no_temporary_files(&self) {
        assert!(!std::fs::read_dir(&self.directory)
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".nora-kiro-")));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn renewed(session: &SourceSession) -> Credential {
    let mut credential = session.credential().clone();
    credential.access = "renewed-access".into();
    credential.refresh = "renewed-refresh".into();
    credential.expires = session::now() + 3_600_000;
    credential.email = "person@example.test".into();
    credential.user_id = "verified-user".into();
    credential.identity = credential.legacy_identity();
    credential
}

#[test]
fn cli_renewal_updates_only_the_selected_slot_and_preserves_owner_metadata() {
    let fixture = Fixture::cli();
    let untouched = json!({"access_token":"","custom":"other-slot"});
    fixture.write_row("kirocli:odic:token", &untouched);
    let mut source = fixture.open(&SourceKind::Cli);
    let next = renewed(&source);
    source.bind_identity(&next.saved()).unwrap();
    source.replace(&next).unwrap();
    let saved: Value = serde_json::from_str(&fixture.row("kirocli:social:token")).unwrap();
    assert_eq!(saved["access_token"], "renewed-access");
    assert_eq!(saved["refresh_token"], "renewed-refresh");
    assert_eq!(session::timestamp(saved.get("expires_at")), next.expires);
    assert_eq!(saved["unknown"], json!({"keep":true}));
    assert!(saved.get("email").is_none());
    assert!(saved.get("userId").is_none());
    assert_eq!(fixture.row("kirocli:odic:token"), untouched.to_string());
    assert_eq!(source.credential().access, next.access);
    assert_eq!(
        session::read_source(&SourceKind::Cli, fixture.path.to_str().unwrap())
            .unwrap()
            .refresh,
        next.refresh
    );
}

#[test]
fn ide_renewal_atomically_preserves_unknown_fields_and_writes_private_permissions() {
    let fixture = Fixture::ide();
    let mut source = fixture.open(&SourceKind::Ide);
    let next = renewed(&source);
    source.bind_identity(&next.saved()).unwrap();
    source.replace(&next).unwrap();
    let saved: Value = serde_json::from_slice(&std::fs::read(&fixture.path).unwrap()).unwrap();
    assert_eq!(saved["accessToken"], next.access);
    assert_eq!(saved["refreshToken"], next.refresh);
    assert_eq!(session::timestamp(saved.get("expiresAt")), next.expires);
    assert_eq!(saved["unknown"], json!({"keep":true}));
    assert!(saved.get("userId").is_none());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&fixture.path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    fixture.no_temporary_files();
}

#[test]
fn a_cli_owner_rotation_during_network_renewal_is_never_overwritten() {
    let fixture = Fixture::cli();
    let mut source = fixture.open(&SourceKind::Cli);
    let next = renewed(&source);
    let owner = json!({"access_token":"owner-private-access","refresh_token":"owner-private-refresh","profile_arn":PROFILE});
    fixture.write_row("kirocli:social:token", &owner);
    let error = source.replace(&next).err().unwrap();
    assert!(!error.message.contains("private"));
    assert_eq!(fixture.row("kirocli:social:token"), owner.to_string());
}

#[test]
fn switching_to_a_higher_priority_cli_slot_during_renewal_does_not_write_the_old_slot() {
    let fixture = Fixture::cli();
    fixture.write_row("kirocli:social:token", &json!({"access_token":""}));
    let initial = json!({"access_token":"idc-access","refresh_token":"idc-refresh","start_url":"https://tenant.awsapps.com/start","profile_arn":PROFILE});
    fixture.write_row("kirocli:odic:token", &initial);
    let mut source = fixture.open(&SourceKind::Cli);
    let next = renewed(&source);
    fixture.write_row(
        "kirocli:social:token",
        &json!({"access_token":"different-owner","refresh_token":"different-refresh"}),
    );
    assert!(source.replace(&next).is_err());
    assert_eq!(fixture.row("kirocli:odic:token"), initial.to_string());
}

#[test]
fn changed_cli_client_registration_prevents_saving_a_token_from_the_old_registration() {
    let fixture = Fixture::cli();
    fixture.write_row("kirocli:social:token", &json!({"access_token":""}));
    let initial =
        json!({"access_token":"idc-access","refresh_token":"idc-refresh","profile_arn":PROFILE});
    fixture.write_row("kirocli:odic:token", &initial);
    fixture.write_row(
        "kirocli:odic:device-registration",
        &json!({"client_id":"client","client_secret":"old-secret"}),
    );
    let mut source = fixture.open(&SourceKind::Cli);
    let next = renewed(&source);
    fixture.write_row(
        "kirocli:odic:device-registration",
        &json!({"client_id":"client","client_secret":"new-secret"}),
    );
    assert!(source.replace(&next).is_err());
    assert_eq!(fixture.row("kirocli:odic:token"), initial.to_string());
}

#[test]
fn ide_owner_changes_are_preserved_and_failed_save_cleans_its_temporary_file() {
    let fixture = Fixture::ide();
    let mut source = fixture.open(&SourceKind::Ide);
    let next = renewed(&source);
    let owner =
        json!({"accessToken":"owner-access","refreshToken":"owner-refresh","profileArn":PROFILE});
    std::fs::write(&fixture.path, owner.to_string()).unwrap();
    assert!(source.replace(&next).is_err());
    assert_eq!(
        std::fs::read_to_string(&fixture.path).unwrap(),
        owner.to_string()
    );
    fixture.no_temporary_files();
}

#[test]
fn another_verified_user_or_billing_profile_cannot_replace_the_bound_account() {
    for kind in ["cli", "ide"] {
        let fixture = if kind == "cli" {
            Fixture::cli()
        } else {
            Fixture::ide()
        };
        let before = std::fs::read(&fixture.path).unwrap();
        let mut source = fixture.open(&SourceKind::from_reference(&json!({"kind":kind})).unwrap());
        let expected = renewed(&source);
        source.bind_identity(&expected.saved()).unwrap();
        let mut wrong = expected.clone();
        wrong.user_id = "another-user".into();
        assert!(source.replace(&wrong).is_err());
        wrong = expected.clone();
        wrong.profile.push_str("-different-billing-profile");
        assert!(source.replace(&wrong).is_err());
        assert_eq!(std::fs::read(&fixture.path).unwrap(), before);
        fixture.no_temporary_files();
    }
}

#[test]
fn server_renewal_saves_its_new_profile_before_identity_lookup_without_rebinding_the_old_nora_account(
) {
    for kind in ["cli", "ide"] {
        let fixture = if kind == "cli" {
            Fixture::cli()
        } else {
            Fixture::ide()
        };
        let mut source = fixture.open(&SourceKind::from_reference(&json!({"kind":kind})).unwrap());
        let expected = renewed(&source).saved();
        source.bind_identity(&expected).unwrap();
        let mut server = source.credential().clone();
        server.access = "server-new-access".into();
        server.refresh = "server-rotated-refresh".into();
        server.expires = session::now() + 3_600_000;
        server.profile.push_str("-server-updated-profile");
        assert!(server.account_identity().is_none());
        assert!(source.replace(&server).is_err());
        source.replace_refreshed(&server).unwrap();
        let current = session::read_source(
            &SourceKind::from_reference(&json!({"kind":kind})).unwrap(),
            fixture.path.to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(current.profile, server.profile);
        assert_eq!(current.access, server.access);
        assert_eq!(current.refresh, server.refresh);
        assert_eq!(current.expires, server.expires);
        let mut identified = server;
        identified.email = "person@example.test".into();
        identified.user_id = "verified-user".into();
        identified.identity = identified.legacy_identity();
        assert!(!identified.matches_saved_identity(&expected));
        assert!(source.reload().is_err());
        fixture.no_temporary_files();
    }
}

#[test]
fn server_profile_changes_do_not_bypass_owner_change_or_private_credential_checks() {
    let fixture = Fixture::ide();
    let mut source = fixture.open(&SourceKind::Ide);
    let mut server = renewed(&source);
    server.profile.push_str("-server-profile");
    let mut invalid = server.clone();
    invalid.method = "idc".into();
    assert!(source.replace_refreshed(&invalid).is_err());
    invalid = server.clone();
    invalid.access.clear();
    assert!(source.replace_refreshed(&invalid).is_err());
    invalid = server.clone();
    invalid.expires = session::now() - 1;
    assert!(source.replace_refreshed(&invalid).is_err());
    let owner = json!({"accessToken":"owner-current-access","refreshToken":"owner-current-refresh","authMethod":"social","profileArn":PROFILE});
    std::fs::write(&fixture.path, owner.to_string()).unwrap();
    assert!(source.replace_refreshed(&server).is_err());
    assert_eq!(
        std::fs::read_to_string(&fixture.path).unwrap(),
        owner.to_string()
    );
    fixture.no_temporary_files();
}

#[test]
fn known_scope_conflicts_are_rejected_before_a_remote_refresh_is_attempted() {
    let fixture = Fixture::ide();
    let mut source = fixture.open(&SourceKind::Ide);
    let mut wrong = renewed(&source).saved();
    wrong["profileArn"] = json!("another-profile");
    assert!(source.bind_identity(&wrong).is_err());
    let mut email = renewed(&source);
    email.user_id.clear();
    email.realm = "https://another-tenant.awsapps.com/start".into();
    assert!(source.bind_identity(&email.saved()).is_err());
}

fn magpie_fixture() -> Fixture {
    Fixture::new(
        "magpie",
        json!({
            "kiro#a":{"type":"oauth","method":"social","access":"a-access","refresh":"a-refresh","expires":1_600_000_000_000_i64,"profileArn":PROFILE,"accountId":"a@example.test (Google)","uid":"google","loginProvider":"Google","unknown":{"preserve":true}},
            "kiro#b":{"type":"oauth","method":"social","access":"b-access","refresh":"b-refresh","expires":1_600_000_000_000_i64,"profileArn":PROFILE,"accountId":"b@example.test (GitHub)","uid":"github","loginProvider":"GitHub"},
            "kiro":{"type":"oauth","source":"kiro-cli","access":"","refresh":"","expires":0},
            "openai":{"type":"oauth","access":"unrelated-provider-access","custom":[1,2,3]}
        }),
    )
}
fn magpie_document(fixture: &Fixture) -> Value {
    serde_json::from_slice(&std::fs::read(&fixture.path).unwrap()).unwrap()
}

#[test]
fn magpie_renewal_updates_only_its_slot_with_millisecond_expiry_and_preserves_other_accounts() {
    let fixture = magpie_fixture();
    let before = magpie_document(&fixture);
    let mut source = fixture.open(&SourceKind::Magpie(
        crate::source_kind::MagpieAccount::new("kiro#a").unwrap(),
    ));
    let mut next = renewed(&source);
    next.profile.push_str("-server-profile");
    source.replace_refreshed(&next).unwrap();
    let saved = magpie_document(&fixture);
    assert_eq!(saved["kiro#a"]["access"], next.access);
    assert_eq!(saved["kiro#a"]["refresh"], next.refresh);
    assert_eq!(saved["kiro#a"]["expires"], next.expires);
    assert_eq!(saved["kiro#a"]["profileArn"], next.profile);
    for field in ["accountId", "uid", "loginProvider", "unknown"] {
        assert_eq!(saved["kiro#a"][field], before["kiro#a"][field]);
    }
    for key in ["kiro#b", "kiro", "openai"] {
        assert_eq!(saved[key], before[key]);
    }
    assert!(saved["kiro#a"].get("email").is_none());
    assert!(saved["kiro#a"].get("userId").is_none());
    assert!(!PathBuf::from(format!("{}.lock", fixture.path.to_string_lossy())).exists());
    fixture.no_temporary_files();
}

#[test]
fn magpie_rebases_an_unchanged_slot_on_owner_updates_to_other_entries() {
    let fixture = magpie_fixture();
    let mut source = fixture.open(&SourceKind::Magpie(
        crate::source_kind::MagpieAccount::new("kiro#a").unwrap(),
    ));
    let next = renewed(&source);
    let mut owner = magpie_document(&fixture);
    owner["kiro#b"]["access"] = json!("owner-new-b-access");
    owner["new-provider"] = json!({"private":"keep-unrelated-entry"});
    let temporary = fixture.directory.join("owner-write.json");
    std::fs::write(&temporary, owner.to_string()).unwrap();
    std::fs::rename(&temporary, &fixture.path).unwrap();
    assert_eq!(source.state(&next).unwrap(), SourceState::Original);
    source.replace_refreshed(&next).unwrap();
    let saved = magpie_document(&fixture);
    assert_eq!(saved["kiro#b"], owner["kiro#b"]);
    assert_eq!(saved["new-provider"], owner["new-provider"]);
    assert_eq!(saved["kiro#a"]["access"], next.access);
}

#[test]
fn magpie_display_uid_and_plan_updates_do_not_discard_a_pending_rotation() {
    let fixture = magpie_fixture();
    let mut old = magpie_document(&fixture);
    old["kiro#a"].as_object_mut().unwrap().remove("uid");
    old["kiro#a"]["accountId"] = json!("a@example.test");
    std::fs::write(&fixture.path, old.to_string()).unwrap();
    let mut source = fixture.open(&SourceKind::Magpie(
        crate::source_kind::MagpieAccount::new("kiro#a").unwrap(),
    ));
    let next = renewed(&source);
    let mut owner = magpie_document(&fixture);
    owner["kiro#a"]["accountId"] = json!("a@example.test (Google)");
    owner["kiro#a"]["uid"] = json!("google");
    owner["kiro#a"]["plan"] = json!("New plan");
    owner["kiro#a"]["unknown"] = json!({"changed-by-owner":true});
    let temporary = fixture.directory.join("owner-metadata.json");
    std::fs::write(&temporary, owner.to_string()).unwrap();
    std::fs::rename(&temporary, &fixture.path).unwrap();
    assert_eq!(source.state(&next).unwrap(), SourceState::Original);
    source.replace_refreshed(&next).unwrap();
    let saved = magpie_document(&fixture);
    for field in ["accountId", "uid", "plan", "unknown"] {
        assert_eq!(saved["kiro#a"][field], owner["kiro#a"][field]);
    }
    assert_eq!(saved["kiro#a"]["access"], next.access);
    assert_eq!(saved["kiro#a"]["refresh"], next.refresh);
}

#[test]
fn magpie_profile_login_method_and_realm_changes_invalidate_the_original_slot() {
    for (field, value) in [
        ("profileArn", json!("different-profile")),
        ("method", json!("idc")),
        ("realm", json!("https://different-tenant.example.test")),
        ("loginProvider", json!("GitHub")),
    ] {
        let fixture = magpie_fixture();
        let mut source = fixture.open(&SourceKind::Magpie(
            crate::source_kind::MagpieAccount::new("kiro#a").unwrap(),
        ));
        let next = renewed(&source);
        let mut owner = magpie_document(&fixture);
        owner["kiro#a"][field] = value;
        std::fs::write(&fixture.path, owner.to_string()).unwrap();
        assert_eq!(source.state(&next).unwrap(), SourceState::Changed);
        assert!(source.replace_refreshed(&next).is_err());
        assert_eq!(magpie_document(&fixture), owner);
    }
}

#[test]
fn magpie_owner_replacing_or_removing_the_selected_login_prevents_a_stale_write() {
    let fixture = magpie_fixture();
    let mut source = fixture.open(&SourceKind::Magpie(
        crate::source_kind::MagpieAccount::new("kiro#a").unwrap(),
    ));
    let next = renewed(&source);
    let mut owner = magpie_document(&fixture);
    owner["kiro#a"]["access"] = json!("different-owner-access");
    owner["kiro#a"]["refresh"] = json!("different-owner-refresh");
    std::fs::write(&fixture.path, owner.to_string()).unwrap();
    assert_eq!(source.state(&next).unwrap(), SourceState::Changed);
    assert!(source.replace_refreshed(&next).is_err());
    assert_eq!(magpie_document(&fixture), owner);
    owner.as_object_mut().unwrap().remove("kiro#a");
    std::fs::write(&fixture.path, owner.to_string()).unwrap();
    assert_eq!(source.state(&next).unwrap(), SourceState::SignedOut);
    assert!(source.replace_refreshed(&next).is_err());
    assert_eq!(magpie_document(&fixture), owner);
    std::fs::remove_file(&fixture.path).unwrap();
    assert!(source.replace_refreshed(&next).is_err());
    assert!(!fixture.path.exists());
    fixture.no_temporary_files();
}

#[test]
fn magpie_slots_share_one_file_lock_and_both_renewals_survive() {
    use std::sync::mpsc;
    let fixture = magpie_fixture();
    let mut first = fixture.open(&SourceKind::Magpie(
        crate::source_kind::MagpieAccount::new("kiro#a").unwrap(),
    ));
    let next_a = renewed(&first);
    assert!(first.release_for_verification(&next_a).is_err());
    let path = fixture.path.clone();
    let root = fixture.locks.clone();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        ready_tx.send(()).unwrap();
        let mut second = SourceSession::open(
            &SourceKind::Magpie(crate::source_kind::MagpieAccount::new("kiro#b").unwrap()),
            path.to_str().unwrap(),
            &root,
        )
        .unwrap()
        .unwrap();
        let mut next = renewed(&second);
        next.access = "second-renewed-access".into();
        next.refresh = "second-renewed-refresh".into();
        second.replace_refreshed(&next).unwrap();
        done_tx.send(()).unwrap();
    });
    ready_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert!(done_rx
        .recv_timeout(std::time::Duration::from_millis(80))
        .is_err());
    first.replace_refreshed(&next_a).unwrap();
    first.release_for_verification(&next_a).unwrap();
    done_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    worker.join().unwrap();
    let saved = magpie_document(&fixture);
    assert_eq!(saved["kiro#a"]["access"], next_a.access);
    assert_eq!(saved["kiro#b"]["access"], "second-renewed-access");
    assert_eq!(saved["kiro#b"]["refresh"], "second-renewed-refresh");
    assert_eq!(first.state(&next_a).unwrap(), SourceState::Written);
    assert!(first.replace_refreshed(&next_a).is_err());
    let mut owner = saved;
    owner["kiro#a"]["access"] = json!("owner-changed-access");
    std::fs::write(&fixture.path, owner.to_string()).unwrap();
    assert_eq!(first.state(&next_a).unwrap(), SourceState::Changed);
    assert!(first.release_for_verification(&next_a).is_err());
    assert_eq!(magpie_document(&fixture), owner);
}

#[test]
fn magpie_write_back_waits_for_its_owners_lock_and_keeps_the_owners_other_changes() {
    use std::sync::mpsc;
    let fixture = magpie_fixture();
    let owner_lock = magpie::WriteLock::acquire(&fixture.path).unwrap();
    let mut source = fixture.open(&SourceKind::Magpie(
        crate::source_kind::MagpieAccount::new("kiro#a").unwrap(),
    ));
    let next = renewed(&source);
    let expected_access = next.access.clone();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        ready_tx.send(()).unwrap();
        source.replace_refreshed(&next).unwrap();
        done_tx.send(()).unwrap();
    });
    ready_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert!(done_rx
        .recv_timeout(std::time::Duration::from_millis(80))
        .is_err());
    let mut owner = magpie_document(&fixture);
    owner["openai"]["custom"] = json!(["owner-wrote-this"]);
    std::fs::write(&fixture.path, owner.to_string()).unwrap();
    drop(owner_lock);
    done_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    worker.join().unwrap();
    let saved = magpie_document(&fixture);
    assert_eq!(saved["openai"], owner["openai"]);
    assert_eq!(saved["kiro#a"]["access"], expected_access);
    fixture.no_temporary_files();
}

#[test]
fn missing_or_signed_out_is_empty_but_broken_source_is_an_error() {
    let fixture = Fixture::ide();
    std::fs::remove_file(&fixture.path).unwrap();
    assert!(SourceSession::open(
        &SourceKind::Ide,
        fixture.path.to_str().unwrap(),
        &fixture.locks
    )
    .unwrap()
    .is_none());
    std::fs::write(&fixture.path, "{}").unwrap();
    assert!(SourceSession::open(
        &SourceKind::Ide,
        fixture.path.to_str().unwrap(),
        &fixture.locks
    )
    .unwrap()
    .is_none());
    std::fs::write(&fixture.path, "broken-private-content").unwrap();
    let error = SourceSession::open(
        &SourceKind::Ide,
        fixture.path.to_str().unwrap(),
        &fixture.locks,
    )
    .err()
    .unwrap();
    assert!(!error.message.contains("private"));
    assert!(SourceSession::open(&SourceKind::Ide, "relative-path", &fixture.locks).is_err());
}

#[test]
fn reload_observes_a_successful_owner_refresh_without_releasing_the_session_lock() {
    let fixture = Fixture::cli();
    let mut source = fixture.open(&SourceKind::Cli);
    let owner =
        json!({"access_token":"owner-new","refresh_token":"owner-refresh","profile_arn":PROFILE});
    fixture.write_row("kirocli:social:token", &owner);
    let reloaded = source.reload().unwrap().unwrap();
    assert_eq!(reloaded.access, "owner-new");
    let next = renewed(&source);
    source.replace(&next).unwrap();
    assert_eq!(source.credential().access, "renewed-access");
}

#[test]
fn pending_rotation_can_distinguish_the_original_source_its_saved_pair_and_an_owner_change() {
    for kind in ["cli", "ide"] {
        let fixture = if kind == "cli" {
            Fixture::cli()
        } else {
            Fixture::ide()
        };
        let source = fixture.open(&SourceKind::from_reference(&json!({"kind":kind})).unwrap());
        let next = renewed(&source);
        assert_eq!(source.state(&next).unwrap(), SourceState::Original);
        if kind == "cli" {
            fixture.write_row("kirocli:social:token",&json!({"access_token":next.access,"refresh_token":next.refresh,"expires_at":expiry(next.expires).unwrap(),"profile_arn":next.profile,"region":next.region}));
        } else {
            std::fs::write(&fixture.path,json!({"accessToken":next.access,"refreshToken":next.refresh,"expiresAt":expiry(next.expires).unwrap(),"profileArn":next.profile,"region":next.region,"authMethod":"social"}).to_string()).unwrap();
        }
        assert_eq!(source.state(&next).unwrap(), SourceState::Written);
        if kind == "cli" {
            fixture.write_row("kirocli:social:token",&json!({"access_token":"owner-other","refresh_token":"owner-other-refresh","profile_arn":PROFILE}));
        } else {
            std::fs::write(&fixture.path,json!({"accessToken":"owner-other","refreshToken":"owner-other-refresh","profileArn":PROFILE}).to_string()).unwrap();
        }
        assert_eq!(source.state(&next).unwrap(), SourceState::Changed);
        std::fs::remove_file(&fixture.path).unwrap();
        assert_eq!(source.state(&next).unwrap(), SourceState::SignedOut);
    }
}

#[test]
fn deleting_the_source_during_renewal_fails_without_recreating_it() {
    for kind in ["cli", "ide"] {
        let fixture = if kind == "cli" {
            Fixture::cli()
        } else {
            Fixture::ide()
        };
        let mut source = fixture.open(&SourceKind::from_reference(&json!({"kind":kind})).unwrap());
        let next = renewed(&source);
        std::fs::remove_file(&fixture.path).unwrap();
        assert!(source.replace(&next).is_err());
        assert!(!fixture.path.exists());
        fixture.no_temporary_files();
    }
}

#[cfg(unix)]
#[test]
fn source_lock_child_process() {
    let Ok(root) = std::env::var("NORA_KIRO_TEST_LOCK_ROOT") else {
        return;
    };
    let path = std::env::var("NORA_KIRO_TEST_SOURCE_PATH").unwrap();
    println!("waiting");
    std::io::stdout().flush().unwrap();
    let _source = SourceSession::open(&SourceKind::Ide, &path, Path::new(&root))
        .unwrap()
        .unwrap();
    println!("acquired");
    std::io::stdout().flush().unwrap();
}

#[cfg(unix)]
#[test]
fn a_source_session_blocks_another_process_until_the_whole_refresh_save_is_finished() {
    use std::io::{BufRead, BufReader};
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};
    let fixture = Fixture::ide();
    let source = fixture.open(&SourceKind::Ide);
    assert_eq!(
        std::fs::metadata(&fixture.locks)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "source::tests::source_lock_child_process",
            "--nocapture",
        ])
        .env("NORA_KIRO_TEST_LOCK_ROOT", &fixture.locks)
        .env("NORA_KIRO_TEST_SOURCE_PATH", &fixture.path)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    while !line.contains("waiting") {
        line.clear();
        assert!(output.read_line(&mut line).unwrap() > 0);
    }
    assert!(child.try_wait().unwrap().is_none());
    drop(source);
    line.clear();
    while !line.contains("acquired") {
        line.clear();
        assert!(output.read_line(&mut line).unwrap() > 0);
    }
    assert!(child.wait().unwrap().success());
}

#[cfg(unix)]
#[test]
fn changing_a_source_symlink_does_not_overwrite_its_old_target() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::ide();
    let alias = fixture.directory.join("source-alias.json");
    symlink(&fixture.path, &alias).unwrap();
    let mut source = SourceSession::open(&SourceKind::Ide, alias.to_str().unwrap(), &fixture.locks)
        .unwrap()
        .unwrap();
    let next = renewed(&source);
    let before = std::fs::read(&fixture.path).unwrap();
    let another = fixture.directory.join("another.json");
    std::fs::write(&another, "{}").unwrap();
    std::fs::remove_file(&alias).unwrap();
    symlink(&another, &alias).unwrap();
    assert!(source.replace(&next).is_err());
    assert_eq!(std::fs::read(&fixture.path).unwrap(), before);
    assert_eq!(std::fs::read_to_string(&another).unwrap(), "{}");
}
