#[test]
fn packaged_identity_matches_the_executable() {
    let text = include_str!("../nora-provider.json");
    assert!(text.contains("\"id\": \"commandcode-plan\""));
    assert!(text.contains("\"type\": \"native\""));
}

#[test]
fn cli_sign_in_needs_no_path_and_paid_option_explains_its_confirmation() {
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("../nora-provider.json")).unwrap();
    let cli = manifest["authMethods"]
        .as_array()
        .unwrap()
        .iter()
        .find(|method| method["id"] == "cli")
        .unwrap();
    assert_eq!(cli["fields"][0]["required"], false);
    assert_eq!(cli["fields"][0]["placeholder"], "~/.commandcode/auth.json");
    let option = &manifest["config"]["options"][0];
    assert_eq!(option["id"], "zdr");
    assert_eq!(option["default"], false);
    assert!(option["confirm"].as_str().unwrap().contains("cost more"));
    assert!(option["confirm"]
        .as_str()
        .unwrap()
        .contains("fails instead of falling back"));
}
