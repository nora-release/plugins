#[test]
fn international_binary_and_manifest_share_the_expected_provider_identity() {
    assert_eq!(
        nora_provider_workbuddy::Site::International.id(),
        "workbuddy-ai"
    );
    let manifest = include_str!("../nora-provider.json");
    assert!(manifest.contains("https://www.workbuddy.ai/v2"));
    assert!(manifest.contains("read only"));
}
