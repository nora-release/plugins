#[test]
fn pass_package_uses_the_shared_native_provider_with_explicit_api_authentication() {
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("../nora-plugin.json")).unwrap();
    assert_eq!(manifest["id"], "clinepass");
    assert_eq!(
        manifest["contributes"]["provider"]["authMethods"][0]["id"],
        "apiKey"
    );
    assert_eq!(
        manifest["contributes"]["provider"]["baseURL"],
        "https://api.cline.bot/api/v1"
    );
    for model in manifest["contributes"]["provider"]["models"]
        .as_array()
        .unwrap()
    {
        assert!(model["id"].as_str().unwrap().starts_with("cline-pass/"));
    }
}
