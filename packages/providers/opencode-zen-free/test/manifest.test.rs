#[test]
fn packaged_identity_matches_the_executable() {
    let text = include_str!("../nora-plugin.json");
    assert!(text.contains("\"id\": \"opencode-zen-free\""));
}

#[test]
fn public_access_declares_that_authorize_supplies_the_key() {
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("../nora-plugin.json")).unwrap();
    let method = &manifest["contributes"]["provider"]["authMethods"][0];
    assert_eq!(method["id"], "public");
    assert_eq!(method["type"], "apiKey");
    assert_eq!(method["keyless"], true);
    assert_eq!(method["fields"], serde_json::json!([]));
}
