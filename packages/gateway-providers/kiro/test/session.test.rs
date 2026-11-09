use super::*;
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
fn login_provider_survives_saved_round_trips_and_has_a_canonical_identity() {
    let mut saved = verified().saved();
    saved["loginProvider"] = json!("Google");
    let google = Credential::from_saved(&saved).unwrap();
    let google_identity = google.account_identity().unwrap();
    assert_ne!(google_identity, saved["accountIdentity"]);
    assert_eq!(google.saved()["loginProvider"], "Google");
    assert_eq!(
        Credential::from_saved(&google.saved())
            .unwrap()
            .account_identity(),
        Some(google_identity.clone())
    );
    assert!(google.matches_saved_identity(&saved));
    saved["loginProvider"] = json!("  GOOGLE  ");
    assert_eq!(
        Credential::from_saved(&saved).unwrap().account_identity(),
        Some(google_identity)
    );
    saved["loginProvider"] = json!("Github");
    let github = Credential::from_saved(&saved).unwrap();
    assert_ne!(google.account_identity(), github.account_identity());
    assert!(!google.matches_saved_identity(&github.saved()));
    assert!(!github.matches_saved_identity(&google.saved()));
}

#[test]
fn known_login_provider_cannot_downgrade_or_change_after_identity_migration() {
    let mut old = verified().saved();
    old["loginProvider"] = json!("Google");
    let next = Credential::from_saved(&old).unwrap().saved();
    for value in [Value::Null, json!(""), json!("Github")] {
        let mut changed = next.clone();
        changed["loginProvider"] = value;
        assert!(Credential::from_saved(&changed).is_err());
    }
    let unknown = verified();
    assert!(!unknown.matches_saved_identity(&next));
    assert_eq!(unknown.saved()["label"], "person@example.test");
}

#[test]
fn login_provider_legacy_migration_keeps_unknown_sources_unqualified() {
    let mut old = verified().saved();
    old.as_object_mut().unwrap().remove("accountIdentity");
    old.as_object_mut().unwrap().remove("extras");
    old["loginProvider"] = json!("Google");
    let owned = Credential::from_saved(&old).unwrap();
    assert!(owned.matches_saved_identity(&old));
    let mut wrong_way = old.clone();
    wrong_way["loginProvider"] = json!("Github");
    assert!(!owned.matches_saved_identity(&wrong_way));
    old.as_object_mut().unwrap().remove("loginProvider");
    let unknown = Credential::from_saved(&old).unwrap();
    assert_eq!(unknown.account_identity(), verified().account_identity());
    assert!(unknown.matches_saved_identity(&old));
    assert_ne!(owned.account_identity(), unknown.account_identity());
}
