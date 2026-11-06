#[test]
fn cn_has_its_own_hosts_enterprise_method_and_no_bundled_model_catalog() {
    let manifest = include_str!("../nora-plugin.json");
    assert!(manifest.contains("gateway.qoder.com.cn"));
    assert!(manifest.contains("\"id\": \"vpc\""));
    assert!(!manifest.contains("\"models\""));
    assert!(!manifest.contains("api3.qoder.sh"));
}
