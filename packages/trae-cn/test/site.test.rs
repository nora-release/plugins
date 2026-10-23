use super::*;
#[test]
fn only_owned_https_hosts_are_used_for_jwts_and_region_selects_model_and_billing_hosts() {
    for value in [
        "http://coresg-normal.trae.ai",
        "https://attacker.example",
        "https://user:pass@api.trae.cn",
        "https://api.trae.cn?q=token",
        "https://api.trae.cn/#secret",
    ] {
        assert!(host(value).is_none(), "{value}");
    }
    assert_eq!(
        Site::Global.api(&json!({"region":"US"})),
        "https://coreva-normal.trae.ai"
    );
    assert_eq!(
        Site::Global.pay(&json!({"region":"US-east"})),
        "https://api-us-east.trae.ai"
    );
    assert_eq!(
        Site::Global.api(&json!({"api":"https://tenant.trae.ai/proxy"})),
        "https://tenant.trae.ai/proxy"
    );
    assert_eq!(
        Site::China.api(&json!({"api":"https://api.trae.cn"})),
        "https://trae-api-cn.mchost.guru"
    );
    assert_eq!(
        Site::China.api(&json!({"api":"https://tenant.trae.cn"})),
        "https://tenant.trae.cn"
    );
    assert_eq!(
        Site::China.auth_host(&json!({"authHost":"https://evil.trae.ai"})),
        "https://api.trae.cn"
    );
}
#[test]
fn chat_headers_keep_the_account_device_and_the_clients_current_build() {
    let credentials = crate::tests::credentials();
    let headers = ide_headers(&credentials);
    assert_eq!(headers["authorization"], "Cloud-IDE-JWT jwt-one");
    assert_eq!(headers["x-device-id"], "1234567890123456");
    assert_eq!(headers["x-app-version"], "0.1.69");
    assert_ne!(
        headers["x-request-id"],
        ide_headers(&credentials)["x-request-id"]
    );
}
