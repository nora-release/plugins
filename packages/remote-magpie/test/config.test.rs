use super::*;
#[test]
fn every_supported_address_suffix_normalizes_to_the_same_gateway() {
    for suffix in [
        "",
        "/",
        "/v1",
        "/v1/messages/count_tokens",
        "/v1/messages",
        "/v1/chat/completions",
        "/v1/responses",
        "/v1/systemone",
        "/v1/embeddings",
        "/v1/rerank",
        "/v1/audio/transcriptions",
        "/v1/audio/translations",
        "/v1/images/generations",
        "/v1/images/edits",
        "/v1/videos",
    ] {
        assert_eq!(
            normalize(&format!("  gateway.local:3572/mounted{suffix} ")).unwrap(),
            "http://gateway.local:3572/mounted"
        );
    }
    assert_eq!(
        normalize("https://gateway.example/v1").unwrap(),
        "https://gateway.example"
    );
    assert_eq!(normalize("[::1]:3572").unwrap(), "http://[::1]:3572");
}
#[test]
fn addresses_with_secrets_or_ambiguous_targets_are_rejected() {
    for url in [
        "",
        "file:///etc/passwd",
        "https://name:secret@gateway.example",
        "https://gateway.example?key=secret",
        "https://gateway.example/#fragment",
        "http://gateway.example/\n/v1",
    ] {
        assert!(normalize(url).is_err(), "{url}");
    }
}
#[test]
fn key_and_endpoint_are_explicit_and_local_labels_never_claim_a_remote_email() {
    for inputs in [
        json!({"apiKey":"key"}),
        json!({"endpoint":"host:3572"}),
        json!({"endpoint":"host:3572","apiKey":"key\n"}),
    ] {
        assert!(Account::inputs(&inputs).is_err());
    }
    let account =
        Account::inputs(&json!({"endpoint":"host:3572/v1","apiKey":"private","name":"Office"}))
            .unwrap();
    let saved = account.saved();
    assert!(saved.get("email").is_none());
    let restored = Account::credentials(Some(&saved)).unwrap();
    assert_eq!(restored.root, account.root);
    assert_eq!(restored.key, "private");
    assert_eq!(restored.name, "Office");
    let mut changed = saved;
    changed["label"] = json!("Work");
    assert_eq!(Account::credentials(Some(&changed)).unwrap().name, "Work");
}
#[test]
fn only_the_explicit_gateway_and_supported_operation_paths_receive_the_key() {
    let account =
        Account::inputs(&json!({"endpoint":"https://gateway.test/mount","apiKey":"private"}))
            .unwrap();
    for path in [
        "/v1/responses",
        "/v1/messages",
        "/v1/messages/count_tokens",
        "/v1/images/edits",
        "/v1/embeddings",
        "/v1/rerank",
        "/v1/systemone",
        "/v1/videos/opaque/content",
    ] {
        account
            .target(&format!("https://gateway.test/mount{path}"))
            .unwrap();
    }
    for target in [
        "https://other.test/mount/v1/responses",
        "https://gateway.test/mounting/v1/responses",
        "https://gateway.test/mount/v1/models",
        "https://gateway.test/mount/v1/videos/../unknown",
        "https://key@gateway.test/mount/v1/responses",
    ] {
        assert!(account.target(target).is_err(), "{target}");
    }
}
#[test]
fn usage_cache_identity_separates_both_endpoint_and_key() {
    let a = Account::inputs(&json!({"endpoint":"host:3572","apiKey":"one"})).unwrap();
    let b = Account::inputs(&json!({"endpoint":"host:3572","apiKey":"two"})).unwrap();
    let c = Account::inputs(&json!({"endpoint":"host:3573","apiKey":"one"})).unwrap();
    assert_ne!(a.fingerprint(), b.fingerprint());
    assert_ne!(a.fingerprint(), c.fingerprint());
    assert_eq!(a.fingerprint().len(), 64);
}
