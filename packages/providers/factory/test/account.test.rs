use super::*;
use crate::fixture;
#[test]
fn api_keys_never_inherit_oauth_organization_headers() {
    let mut v = fixture::key();
    v["activeOrganizationId"] = json!("wrong-org");
    let account = Account::parse(&v).unwrap();
    assert!(account.key);
    assert!(account.active_org.is_empty());
    assert_eq!(account.identity(), "user-one");
    assert!(network::factory(
        nora_ai::HttpMethod::Get,
        "https://api.factory.ai/test".into(),
        &account
    )
    .headers
    .get("x-factory-org-id")
    .is_none());
}
#[test]
fn whoami_selects_real_region_and_on_prem_host() {
    let mut account = Account::parse(&fixture::saved()).unwrap();
    let mut who = fixture::me();
    who["region"] = json!("eu");
    who["premBaseHostV2"] = json!("models.enterprise.example");
    assert!(account.apply_identity(&who).unwrap());
    assert_eq!(account.base(), network::EU);
    assert_eq!(
        account.llm_base().unwrap(),
        "https://models.enterprise.example"
    );
}
#[test]
fn changed_identity_is_rejected() {
    let mut account = Account::parse(&fixture::saved()).unwrap();
    let mut who = fixture::me();
    who["userId"] = json!("another-user");
    assert!(account.apply_identity(&who).is_err());
}
#[test]
fn on_prem_cannot_disable_tls_or_add_credentials() {
    for host in [
        "http://unsafe.example",
        "https://user:pass@host.test",
        "https://host.test/path",
        "https://host.test?token=private",
        "https://host.test#fragment",
    ] {
        let mut value = fixture::saved();
        value["premBaseHost"] = json!(host);
        assert!(Account::parse(&value).is_err(), "{host}");
    }
}
#[test]
fn workos_claims_supply_real_identity_and_expiry() {
    let account = from_token(&fixture::token()).unwrap();
    assert_eq!(account.user, "user-one");
    assert_eq!(account.expires, 4102444800000);
    assert_eq!(account.org, "workos-one");
    assert_eq!(account.refresh, "private-new-refresh");
}
