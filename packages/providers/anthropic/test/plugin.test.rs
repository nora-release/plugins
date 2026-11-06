use super::*;
#[tokio::test]
async fn account_listing_does_not_run_cli_or_return_private_profile_fields() {
    let plugin = Claude::default();
    let accounts=plugin.accounts(CallContext{ subscription_accounts_enabled: true,account:"work".into(),credentials:Some(json!({"type":"oauth","accountIdentity":{"issuer":"https://claude.ai","subject":"account-a","scope":"org-a"},"email":"person@example.test","access":"private-token","refresh":"private-refresh","extras":{"noraClaudeCLI":{"directory":"/private/profile"}}})),network_proxy:None, ..Default::default()}).await.unwrap();
    assert_eq!(accounts["accounts"][0]["email"], "person@example.test");
    assert!(!accounts.to_string().contains("private"));
}

#[tokio::test]
async fn invalid_tool_choices_and_schemas_fail_before_account_or_proxy_work() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    for invalid in [
        json!({"tools":[],"tool_choice":{"type":"tool","name":"missing"}}),
        json!({"output_config":{"format":{"type":"json_schema","schema":"invalid"}}}),
    ] {
        let plugin = Claude::default();
        let mut body = invalid;
        body["model"] = json!("claude-test");
        body["messages"] = json!([{"role":"user","content":"hello"}]);
        let request:FetchRequest=serde_json::from_value(json!({"credentials":{"type":"oauth"},"networkProxy":{"mode":"proxy","url":"not-a-proxy"},"url":"https://api.anthropic.com/v1/messages","method":"POST","body":STANDARD.encode(body.to_string())})).unwrap();
        let error = plugin
            .fetch(request)
            .await
            .err()
            .expect("invalid request must fail");
        assert_eq!(error.code, "invalid_request");
        assert!(!error.message.contains("Initialize Anthropic"));
        assert!(plugin.config.read().unwrap().is_none());
        assert!(plugin.proxies.lock().await.is_empty());
    }
}
