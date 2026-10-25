use super::*;
#[tokio::test]
async fn account_listing_does_not_run_cli_or_return_private_profile_fields() {
    let plugin = Claude::default();
    let accounts=plugin.accounts(CallContext{ subscription_accounts_enabled: true,account:"work".into(),credentials:Some(json!({"type":"oauth","accountIdentity":{"issuer":"https://claude.ai","subject":"account-a","scope":"org-a"},"email":"person@example.test","access":"private-token","refresh":"private-refresh","extras":{"noraClaudeCLI":{"directory":"/private/profile"}}})),network_proxy:None}).await.unwrap();
    assert_eq!(accounts["accounts"][0]["email"], "person@example.test");
    assert!(!accounts.to_string().contains("private"));
}
