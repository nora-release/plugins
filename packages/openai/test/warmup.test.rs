use super::*;
use serde_json::json;
#[tokio::test]
async fn warmup_requires_explicit_confirmation_before_any_account_or_network_access() {
    let error = OpenAIPlugin::default()
        .call("warmup", json!({}))
        .await
        .unwrap_err();
    assert_eq!(error.code, "invalid_request");
    assert!(error.message.contains("Confirm"), "{error:?}");
}
#[test]
fn warmup_declaration_is_subscription_only() {
    let manifest: Value = serde_json::from_str(include_str!("../nora-provider.json")).unwrap();
    assert_eq!(
        manifest["config"]["accountAutomation"]["warmup"]["method"],
        "warmup"
    );
    assert_eq!(
        manifest["config"]["accountAutomation"]["warmup"]["accountDailyTime"],
        true
    );
}
