use super::*;
#[tokio::test]
async fn account_listing_does_not_run_cli_or_return_private_profile_fields() {
    let plugin = Claude::default();
    let accounts=plugin.accounts(CallContext{account:"work".into(),credentials:Some(json!({"type":"oauth","email":"person@example.test","access":"private-token","refresh":"private-refresh","extras":{"noraClaudeCLI":{"directory":"/private/profile"}}})),network_proxy:None}).await.unwrap();
    assert_eq!(accounts[0]["email"], "person@example.test");
    assert!(!accounts.to_string().contains("private"));
}
