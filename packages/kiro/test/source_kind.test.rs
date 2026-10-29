use super::*;

#[test]
fn source_references_round_trip_without_losing_the_magpie_account() {
    for (kind, method) in [
        (SourceKind::Cli, "cliSource"),
        (SourceKind::Ide, "ideSource"),
        (
            SourceKind::Magpie(MagpieAccount::new("kiro#one").unwrap()),
            "magpieSource",
        ),
    ] {
        let reference = kind.reference("/accounts/current");
        assert_eq!(SourceKind::from_reference(&reference).unwrap(), kind);
        assert_eq!(kind.auth_method(), method);
        let inputs = json!({"sourceAccount":reference.get("account")});
        assert_eq!(SourceKind::from_auth_method(method, &inputs).unwrap(), kind);
        assert_eq!(kind.marker("/accounts/current")["kind"], method);
        assert_eq!(
            kind.marker("/accounts/current").get("account"),
            reference.get("account")
        );
    }
}

#[test]
fn only_known_source_kinds_and_bounded_ascii_kiro_slots_are_accepted() {
    for account in ["kiro", "kiro#slot", "kiro#slot-1_a.b"] {
        assert!(MagpieAccount::new(account).is_ok());
    }
    for account in [
        "openai",
        "kiro#",
        "kiro#../slot",
        "kiro#中文",
        "kiro#x\ny",
        "kiro#x:y",
    ] {
        assert!(MagpieAccount::new(account).is_err());
    }
    assert!(MagpieAccount::new(&format!("kiro#{}", "a".repeat(129))).is_err());
    for reference in [
        json!({"kind":"unknown"}),
        json!({"kind":"magpie"}),
        json!({"kind":"magpie:kiro"}),
        json!({"kind":"magpie","account":"openai"}),
    ] {
        assert!(SourceKind::from_reference(&reference).is_err());
    }
    assert!(SourceKind::from_auth_method("unknown", &json!({})).is_err());
}

#[test]
fn magpie_slots_share_the_document_lock_but_keep_separate_runtime_caches() {
    let a = SourceKind::Magpie(MagpieAccount::new("kiro#a").unwrap());
    let b = SourceKind::Magpie(MagpieAccount::new("kiro#b").unwrap());
    assert_eq!(a.storage_kind(), b.storage_kind());
    assert_ne!(a.cache_key(), b.cache_key());
    assert_ne!(SourceKind::Cli.cache_key(), SourceKind::Ide.cache_key());
}
