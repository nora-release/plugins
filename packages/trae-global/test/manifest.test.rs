#[test]
fn global_is_a_separate_subscription_without_cn_checkin_or_preset_models() {
    let manifest = include_str!("../nora-provider.json");
    assert!(manifest.contains("coresg-normal.trae.ai/v1"));
    assert!(manifest.contains("\"accountActions\": []"));
    assert!(manifest.contains("\"models\": []"));
    assert!(!manifest.contains("api.trae.cn"));
}
