use super::*;
#[test]
fn enterprise_instance_forms_resolve_to_the_same_private_hosts() {
    for input in [
        "Acme",
        "acme.vpc.qoder.com.cn",
        "https://acme.vpc.qoder.com.cn/",
        "acme-openapi.vpc.qoder.com.cn",
        "https://acme-gateway.vpc.qoder.com.cn",
    ] {
        let site = Site::China.deployment(Some(input)).unwrap();
        assert_eq!(site.openapi, "https://acme-openapi.vpc.qoder.com.cn");
        assert_eq!(site.api, "https://acme-gateway.vpc.qoder.com.cn");
        assert_eq!(site.vpc.as_deref(), Some("acme"));
    }
    assert_eq!(instance_name("acme-openapi"), Some("acme-openapi".into()));
}
#[test]
fn invalid_enterprise_records_never_fall_back_to_public_account_hosts() {
    for input in [
        "http://acme.vpc.qoder.com.cn",
        "https://acme.vpc.qoder.com.cn/path",
        "https://acme.vpc.qoder.com.cn?token=x",
        "https://attacker.example",
        "127.0.0.1",
        "acme.vpc.qoder.com.cn.attacker.example",
        "https://user@acme.vpc.qoder.com.cn",
        "-acme",
        "acme/",
    ] {
        assert!(Site::China.deployment(Some(input)).is_err(), "{input}");
    }
    assert!(Site::International.deployment(Some("acme")).is_err());
    let provider = Qoder::new(Site::China);
    assert!(provider.deployment(&json!({"vpc":123})).is_err());
}
