use super::*;
fn temp() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "nora-kiro-test-{}",
        nora_ai::oauth_pkce::random_hex(12)
    ))
}
#[test]
fn source_regions_and_timestamps_are_bounded_and_do_not_accept_endpoints() {
    for input in [
        "https://evil.test",
        "us-east-1/evil",
        "us-east-1@evil",
        "../../us-east-1",
        "us_east_1",
    ] {
        assert!(region(input).is_err());
    }
    assert_eq!(region("eu-central-1").unwrap(), "eu-central-1");
    assert_eq!(timestamp(Some(&json!(1800000000))), 1800000000000);
    assert_eq!(timestamp(Some(&json!(1800000000.0))), 1800000000000);
    assert_eq!(timestamp(Some(&json!(1800000000.125))), 1800000000000);
    assert_eq!(timestamp(Some(&json!(1800000000123.5))), 1800000000123);
    assert_eq!(timestamp(Some(&json!(1e300))), 0);
    assert_eq!(
        timestamp(Some(&json!("2027-01-15T08:00:00Z"))),
        1800000000000
    );
    let cred = Credential {
        region: "eu-west-1".into(),
        ..Default::default()
    };
    assert_eq!(cred.runtime_region().unwrap(), "eu-central-1");
    let cred = Credential {
        profile: "arn:aws:codewhisperer:us-west-2:123:profile/test".into(),
        ..cred
    };
    assert_eq!(cred.runtime_region().unwrap(), "us-west-2");
}
#[test]
fn selected_cli_database_is_read_only_and_uses_the_actual_odic_key() {
    let path = temp();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE auth_kv (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .unwrap();
    db.execute("INSERT INTO auth_kv VALUES (?1,?2)",["kirocli:odic:token",&json!({"access_token":"cli-private","refresh_token":"cli-refresh","expires_at":"2027-01-15T08:00:00Z","region":"eu-west-1","start_url":"https://company.awsapps.com/start"}).to_string()]).unwrap();
    db.execute(
        "INSERT INTO auth_kv VALUES (?1,?2)",
        [
            "kirocli:odic:device-registration",
            &json!({"client_id":"client","client_secret":"private-secret"}).to_string(),
        ],
    )
    .unwrap();
    drop(db);
    let before = std::fs::read(&path).unwrap();
    let cred = read_source(&SourceKind::Cli, path.to_str().unwrap()).unwrap();
    assert_eq!(cred.method, "idc");
    assert!(!cred.builder);
    assert_eq!(cred.client_secret, "private-secret");
    assert_eq!(before, std::fs::read(&path).unwrap());
    std::fs::remove_file(path).unwrap();
}
#[test]
fn selected_ide_registration_is_a_related_file_and_cannot_traverse_directories() {
    let directory = temp();
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("kiro-auth-token.json");
    std::fs::write(
        directory.join("aabb.json"),
        json!({"clientId":"client","clientSecret":"secret"}).to_string(),
    )
    .unwrap();
    std::fs::write(&path,json!({"accessToken":"access","authMethod":"idc","provider":"BuilderId","clientIdHash":"aabb"}).to_string()).unwrap();
    let cred = read_source(&SourceKind::Ide, path.to_str().unwrap()).unwrap();
    assert_eq!(cred.method, "idc");
    assert!(cred.builder);
    std::fs::write(
        &path,
        json!({"accessToken":"access","clientIdHash":"../aabb"}).to_string(),
    )
    .unwrap();
    assert!(read_source(&SourceKind::Ide, path.to_str().unwrap()).is_err());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn social_ide_login_ignores_a_missing_or_broken_registration_from_an_older_login() {
    let directory = temp();
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("kiro-auth-token.json");
    let value = json!({"accessToken":"social-private-access","refreshToken":"social-private-refresh","authMethod":"Social","clientIdHash":"aabb","expiresAt":now()+3_600_000});
    std::fs::write(&path, value.to_string()).unwrap();
    for registration in [None, Some("broken-private-registration")] {
        if let Some(registration) = registration {
            std::fs::write(directory.join("aabb.json"), registration).unwrap();
        }
        let credential = read_source(&SourceKind::Ide, path.to_str().unwrap()).unwrap();
        assert_eq!(credential.method, "social");
        assert_eq!(credential.access, "social-private-access");
        assert!(credential.client_id.is_empty());
        assert!(credential.client_secret.is_empty());
        assert!(credential.fresh());
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), value.to_string());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn an_idc_ide_login_without_readable_registration_remains_usable_as_idc_until_it_needs_renewal() {
    let directory = temp();
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("kiro-auth-token.json");
    std::fs::write(&path,json!({"accessToken":"idc-private-access","authMethod":"IdC","provider":"Enterprise","clientIdHash":"aabb","expiresAt":now()+3_600_000}).to_string()).unwrap();
    for registration in [None, Some("broken-private-registration")] {
        if let Some(registration) = registration {
            std::fs::write(directory.join("aabb.json"), registration).unwrap();
        }
        let credential = read_source(&SourceKind::Ide, path.to_str().unwrap()).unwrap();
        assert_eq!(credential.method, "idc");
        assert_eq!(credential.access, "idc-private-access");
        assert!(credential.client_id.is_empty());
        assert!(credential.client_secret.is_empty());
        assert!(credential.fresh());
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn cli_skips_a_broken_slot_and_keeps_a_valid_idc_access_token_without_registration() {
    let path = temp();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE auth_kv (key TEXT PRIMARY KEY,value TEXT NOT NULL)")
        .unwrap();
    db.execute(
        "INSERT INTO auth_kv VALUES (?1,?2)",
        ["kirocli:social:token", "broken-private-json"],
    )
    .unwrap();
    db.execute("INSERT INTO auth_kv VALUES (?1,?2)",["kirocli:odic:token",&json!({"access_token":"idc-private-access","expires_at":now()+3_600_000,"start_url":"https://tenant.awsapps.com/start"}).to_string()]).unwrap();
    for registration in [None, Some("broken-private-registration")] {
        if let Some(registration) = registration {
            db.execute(
                "INSERT OR REPLACE INTO auth_kv VALUES (?1,?2)",
                ["kirocli:odic:device-registration", registration],
            )
            .unwrap();
        }
        let (credential, key, _) = cli_snapshot(&db).unwrap().unwrap();
        assert_eq!(key, "kirocli:odic:token");
        assert_eq!(credential.method, "idc");
        assert_eq!(credential.access, "idc-private-access");
        assert!(credential.client_id.is_empty());
        assert!(credential.client_secret.is_empty());
        assert!(credential.fresh());
    }
    db.execute(
        "UPDATE auth_kv SET value=?1 WHERE key=?2",
        ["{}", "kirocli:odic:token"],
    )
    .unwrap();
    let error = cli_snapshot(&db).err().unwrap();
    assert!(!error.message.contains("private"));
    db.execute(
        "UPDATE auth_kv SET value=?1 WHERE key=?2",
        ["{}", "kirocli:social:token"],
    )
    .unwrap();
    assert!(cli_snapshot(&db).unwrap().is_none());
    drop(db);
    std::fs::remove_file(path).unwrap();
}
#[test]
fn missing_or_invalid_sources_never_fall_back_to_another_account() {
    assert!(read_source(&SourceKind::Cli, "relative.sqlite3").is_err());
    assert!(SourceKind::from_reference(&json!({"kind":"unknown"})).is_err());
    let path = temp();
    std::fs::write(&path, "private-token invalid JSON").unwrap();
    let error = read_source(&SourceKind::Ide, path.to_str().unwrap())
        .err()
        .unwrap();
    assert!(!error.message.contains("private-token"));
    std::fs::remove_file(path).unwrap();
}
#[test]
fn saved_credentials_retain_the_private_renewal_fields_without_debug_output() {
    let mut credential = Credential::api_key(&json!({"apiKey":"ksk_private"})).unwrap();
    assert!(credential.fresh());
    assert_eq!(credential.token_type(), Some("API_KEY"));
    credential.method = "idc".into();
    credential.client_id = "client".into();
    credential.client_secret = "secret".into();
    credential.refresh = "refresh".into();
    credential.expires = now() + 3600000;
    let next = Credential::from_saved(&credential.saved()).unwrap();
    assert_eq!(next.client_secret, "secret");
    assert_eq!(next.refresh, "refresh");
    assert!(next.fresh());
}
#[test]
fn standard_api_key_credentials_are_normalized_without_overriding_an_explicit_method() {
    for value in [
        json!({"type":"api","key":"ksk_private"}),
        json!({"type":"apiKey","key":"ksk_private"}),
        json!({"authMethod":"apiKey","key":"ksk_private"}),
    ] {
        let credential = Credential::from_saved(&value).unwrap();
        assert_eq!(credential.method, "apikey");
        assert_eq!(credential.token_type(), Some("API_KEY"));
        assert!(credential.fresh());
    }
    let credential =
        Credential::from_saved(&json!({"type":"api","key":"ksk_private","method":"social"}))
            .unwrap();
    assert_eq!(credential.method, "social");
}
#[test]
fn external_idp_cli_source_preserves_its_own_client_and_company_token_endpoint() {
    let path = temp();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE auth_kv (key TEXT PRIMARY KEY,value TEXT NOT NULL)")
        .unwrap();
    db.execute("INSERT INTO auth_kv VALUES (?1,?2)",["kirocli:external-idp:token",&json!({"access_token":"external-private","client_id":"company-client","token_endpoint":"https://company.invalid/token","issuer_url":"https://issuer.company.invalid/tenant","region":"us-east-1"}).to_string()]).unwrap();
    drop(db);
    let credential = read_source(&SourceKind::Cli, path.to_str().unwrap()).unwrap();
    assert_eq!(credential.token_type(), Some("EXTERNAL_IDP"));
    assert_eq!(credential.client_id, "company-client");
    assert_eq!(credential.token_endpoint, "https://company.invalid/token");
    assert_eq!(credential.realm, "https://issuer.company.invalid/tenant");
    std::fs::remove_file(path).unwrap();
}

fn verified() -> Credential {
    let mut credential = Credential {
        access: "private-access".into(),
        method: "social".into(),
        region: "us-east-1".into(),
        profile: "arn:aws:codewhisperer:us-east-1:123456789012:profile/test".into(),
        email: "person@example.test".into(),
        ..Default::default()
    };
    credential.identity = credential.legacy_identity();
    credential
}

#[test]
fn verified_identity_uses_the_user_and_billing_profile_across_login_methods() {
    let mut credential = verified();
    credential.user_id = "stable-service-user".into();
    let expected = credential.account_identity().unwrap();
    assert_eq!(expected["subject"], "user:stable-service-user");
    for method in ["social", "idc", "external-idp", "apikey"] {
        credential.method = method.into();
        assert_eq!(credential.account_identity(), Some(expected.clone()));
    }
    credential.email = "renamed@example.test".into();
    assert_eq!(credential.account_identity(), Some(expected.clone()));
    credential.profile.push_str("-other-billing-profile");
    assert_ne!(credential.account_identity(), Some(expected));
}

#[test]
fn email_fallback_is_case_insensitive_but_cannot_cross_profile_or_enterprise_realm() {
    let mut credential = verified();
    let expected = credential.account_identity().unwrap();
    credential.email = "PERSON@EXAMPLE.TEST".into();
    assert_eq!(credential.account_identity(), Some(expected.clone()));
    credential.profile.push_str("-other");
    assert_ne!(credential.account_identity(), Some(expected));
    credential.realm = "https://tenant-one.awsapps.com/start/".into();
    let tenant_one = credential.account_identity().unwrap();
    credential.realm = "https://tenant-one.awsapps.com/start".into();
    assert_eq!(credential.account_identity(), Some(tenant_one.clone()));
    credential.realm = "https://tenant-two.awsapps.com/start".into();
    assert_ne!(credential.account_identity(), Some(tenant_one));
}

#[test]
fn the_shared_builder_profile_alone_never_identifies_a_user() {
    let mut credential = Credential {
        builder: true,
        profile: BUILDER_PROFILE.into(),
        ..Default::default()
    };
    assert!(credential.account_identity().is_none());
    credential.user_id = "user-one".into();
    let first = credential.account_identity().unwrap();
    credential.user_id = "user-two".into();
    assert_ne!(credential.account_identity(), Some(first.clone()));
    credential.user_id = "user-one".into();
    credential.realm = "https://view.awsapps.com/start/".into();
    assert_eq!(credential.account_identity(), Some(first));
}

#[test]
fn saved_identity_round_trips_and_conflicting_identity_metadata_is_rejected() {
    let mut credential = verified();
    credential.user_id = "service-user".into();
    credential.realm = "https://tenant.awsapps.com/start".into();
    let saved = credential.saved();
    let loaded = Credential::from_saved(&saved).unwrap();
    assert_eq!(loaded.account_identity(), credential.account_identity());
    assert_eq!(loaded.realm, credential.realm);
    assert_eq!(saved["accountIdentity"], saved["extras"]["accountIdentity"]);
    for key in ["profileArn", "userId", "realm"] {
        let mut changed = saved.clone();
        changed[key] = json!("different-private-value");
        assert!(Credential::from_saved(&changed).is_err());
        assert!(!credential.matches_saved_identity(&json!({"accountIdentity":saved["accountIdentity"],"extras":{"accountIdentity":{"issuer":"https://elsewhere.test"}}})));
    }
    let mut changed = saved;
    changed["accountIdentity"]["subject"] = json!("user:someone-else");
    assert!(Credential::from_saved(&changed).is_err());
}

#[test]
fn a_verified_user_id_can_upgrade_an_email_identity_without_weakening_it_later() {
    let mut credential = verified();
    let email_identity = credential.saved();
    credential.user_id = "service-user".into();
    assert!(credential.matches_saved_identity(&email_identity));
    let user_identity = credential.saved();
    credential.user_id.clear();
    assert!(!credential.matches_saved_identity(&user_identity));
    credential.user_id = "different-user".into();
    assert!(!credential.matches_saved_identity(&user_identity));
    credential.profile.push_str("-other");
    assert!(!credential.matches_saved_identity(&email_identity));
}

#[test]
fn legacy_migration_checks_verified_email_hash_and_every_original_scope_that_is_available() {
    let credential = verified();
    let old_owned = json!({"accountId":credential.identity,"email":credential.email,"profileArn":credential.profile});
    assert!(credential.matches_saved_identity(&old_owned));
    let mut changed = old_owned.clone();
    changed["profileArn"] = json!("another-profile");
    assert!(!credential.matches_saved_identity(&changed));
    changed = old_owned.clone();
    changed["email"] = json!("someone-else@example.test");
    assert!(!credential.matches_saved_identity(&changed));
    changed = old_owned.clone();
    changed.as_object_mut().unwrap().remove("profileArn");
    assert!(credential.matches_saved_identity(&changed));
    let mut old_source = old_owned;
    old_source["source"] = json!({"kind":"cli","path":"/fixture/account.sqlite3"});
    assert!(credential.matches_saved_identity(&old_source));
    old_source["realm"] = json!("");
    assert!(credential.matches_saved_identity(&old_source));
}

#[test]
fn legacy_enterprise_sources_acquire_a_scope_once_and_are_strict_after_upgrade() {
    let mut credential = verified();
    credential.method = "idc".into();
    credential.realm = "https://tenant.awsapps.com/start".into();
    let mut old = json!({"accountId":credential.identity,"email":credential.email,"profileArn":credential.profile});
    assert!(credential.matches_saved_identity(&old));
    old["realm"] = json!("https://tenant.awsapps.com/start/");
    assert!(credential.matches_saved_identity(&old));
    old["realm"] = json!("https://other.awsapps.com/start");
    assert!(!credential.matches_saved_identity(&old));
    let upgraded = credential.saved();
    credential.realm = "https://other.awsapps.com/start".into();
    assert!(!credential.matches_saved_identity(&upgraded));
}

#[test]
fn a_verified_user_and_profile_merge_across_aws_start_and_issuer_url_aliases() {
    let mut credential = verified();
    credential.user_id = "service-user".into();
    credential.realm = "https://tenant.awsapps.com/start".into();
    let first = credential.saved();
    credential.realm = "https://identitycenter.amazonaws.com/ssoins-tenant".into();
    assert_eq!(
        credential.account_identity(),
        Some(first["accountIdentity"].clone())
    );
    assert!(credential.matches_saved_identity(&first));
    credential.user_id.clear();
    let email = credential.account_identity();
    credential.realm = "https://tenant.awsapps.com/start".into();
    assert_ne!(credential.account_identity(), email);
}

#[test]
fn external_idp_issuer_fallback_and_query_scope_are_safe_and_do_not_copy_query_secrets() {
    let path = temp();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE auth_kv (key TEXT PRIMARY KEY,value TEXT NOT NULL)")
        .unwrap();
    let mut value = json!({"access_token":"private","client_id":"client","issuer_url":"https://company.invalid/tenant/"});
    db.execute(
        "INSERT INTO auth_kv VALUES (?1,?2)",
        ["kirocli:external-idp:token", &value.to_string()],
    )
    .unwrap();
    assert_eq!(
        read_source(&SourceKind::Cli, path.to_str().unwrap())
            .unwrap()
            .token_endpoint,
        "https://company.invalid/tenant/v1/token"
    );
    value.as_object_mut().unwrap().remove("issuer_url");
    value["token_endpoint"] = json!("https://company.invalid/token?tenant=one&private-query=value");
    db.execute("UPDATE auth_kv SET value=?1", [value.to_string()])
        .unwrap();
    let first = read_source(&SourceKind::Cli, path.to_str().unwrap()).unwrap();
    assert!(first.token_endpoint.contains("private-query=value"));
    assert!(!first.realm.contains("private-query"));
    value["token_endpoint"] = json!("https://company.invalid/token?tenant=two&private-query=value");
    db.execute("UPDATE auth_kv SET value=?1", [value.to_string()])
        .unwrap();
    let second = read_source(&SourceKind::Cli, path.to_str().unwrap()).unwrap();
    assert_ne!(first.realm, second.realm);
    for endpoint in [
        "http://company.invalid/token",
        "https://private-user:private-password@company.invalid/token",
        "https://company.invalid/token#private-fragment",
        "https://company.invalid/\nprivate",
    ] {
        value["token_endpoint"] = json!(endpoint);
        db.execute("UPDATE auth_kv SET value=?1", [value.to_string()])
            .unwrap();
        let error = read_source(&SourceKind::Cli, path.to_str().unwrap())
            .err()
            .unwrap();
        assert!(!error.message.contains("private"));
    }
    drop(db);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn source_discovery_distinguishes_signed_out_from_unreadable_or_malformed_sources() {
    let path = temp();
    assert!(
        read_source_if_signed_in(&SourceKind::Cli, path.to_str().unwrap())
            .unwrap()
            .is_none()
    );
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE auth_kv (key TEXT PRIMARY KEY,value TEXT NOT NULL)")
        .unwrap();
    assert!(
        read_source_if_signed_in(&SourceKind::Cli, path.to_str().unwrap())
            .unwrap()
            .is_none()
    );
    db.execute(
        "INSERT INTO auth_kv VALUES (?1,?2)",
        ["kirocli:social:token", "{\"access_token\":\"\"}"],
    )
    .unwrap();
    assert!(
        read_source_if_signed_in(&SourceKind::Cli, path.to_str().unwrap())
            .unwrap()
            .is_none()
    );
    db.execute("UPDATE auth_kv SET value=?1", ["private-invalid-json"])
        .unwrap();
    let error = read_source_if_signed_in(&SourceKind::Cli, path.to_str().unwrap())
        .err()
        .unwrap();
    assert!(!error.message.contains("private-invalid-json"));
    drop(db);
    std::fs::write(&path, "private-invalid-sqlite").unwrap();
    assert!(read_source_if_signed_in(&SourceKind::Cli, path.to_str().unwrap()).is_err());
    std::fs::write(&path, "{}").unwrap();
    assert!(
        read_source_if_signed_in(&SourceKind::Ide, path.to_str().unwrap())
            .unwrap()
            .is_none()
    );
    std::fs::write(&path, "{\"accessToken\":true}").unwrap();
    assert!(read_source_if_signed_in(&SourceKind::Ide, path.to_str().unwrap()).is_err());
    std::fs::write(&path, "not-json-private-token").unwrap();
    assert!(read_source_if_signed_in(&SourceKind::Ide, path.to_str().unwrap()).is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(read_source_if_signed_in(&SourceKind::Ide, path.to_str().unwrap()).is_err());
    std::fs::remove_dir(path).unwrap();
}

#[test]
fn a_locked_source_database_is_an_error_and_does_not_look_like_signout() {
    let path = temp();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(
        "CREATE TABLE auth_kv (key TEXT PRIMARY KEY,value TEXT NOT NULL); BEGIN EXCLUSIVE",
    )
    .unwrap();
    assert!(read_source_if_signed_in(&SourceKind::Cli, path.to_str().unwrap()).is_err());
    db.execute_batch("ROLLBACK").unwrap();
    assert!(
        read_source_if_signed_in(&SourceKind::Cli, path.to_str().unwrap())
            .unwrap()
            .is_none()
    );
    drop(db);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn source_realm_is_read_from_each_live_source_and_builder_urls_are_normalized() {
    let path = temp();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE auth_kv (key TEXT PRIMARY KEY,value TEXT NOT NULL)")
        .unwrap();
    db.execute(
        "INSERT INTO auth_kv VALUES (?1,?2)",
        [
            "kirocli:odic:token",
            &json!({"access_token":"private","start_url":"https://tenant.awsapps.com/start/"})
                .to_string(),
        ],
    )
    .unwrap();
    let c = read_source(&SourceKind::Cli, path.to_str().unwrap()).unwrap();
    assert_eq!(c.realm, "https://tenant.awsapps.com/start");
    db.execute(
        "UPDATE auth_kv SET value=?1",
        [
            &json!({"access_token":"private","start_url":"https://view.awsapps.com/start/"})
                .to_string(),
        ],
    )
    .unwrap();
    let c = read_source(&SourceKind::Cli, path.to_str().unwrap()).unwrap();
    assert!(c.builder);
    assert!(c.realm.is_empty());
    drop(db);
    std::fs::write(
        &path,
        json!({"accessToken":"private","issuerUrl":"https://tenant.awsapps.com/start/"})
            .to_string(),
    )
    .unwrap();
    assert_eq!(
        read_source(&SourceKind::Ide, path.to_str().unwrap())
            .unwrap()
            .realm,
        "https://tenant.awsapps.com/start"
    );
    std::fs::write(&path,json!({"accessToken":"private","provider":"BuilderId","issuerUrl":"https://view.awsapps.com/start/"}).to_string()).unwrap();
    assert!(read_source(&SourceKind::Ide, path.to_str().unwrap())
        .unwrap()
        .realm
        .is_empty());
    std::fs::remove_file(path).unwrap();
}
