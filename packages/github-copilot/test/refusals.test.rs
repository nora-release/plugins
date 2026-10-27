use super::*;
use nora_provider::ProviderAdapter;
use serde_json::json;

fn credentials(subject: &str, token: &str) -> Value {
    json!({"type":"oauth", "access":token, "extras":{"accountIdentity":{"issuer":"https://github.com", "subject":subject}}})
}
fn entry() -> Value {
    json!({"id":"refused-model", "model_picker_enabled":true, "policy":{"state":"enabled"}, "capabilities":{"type":"chat"}})
}

#[test]
fn refused_models_are_hidden_for_the_same_identity_across_token_renewal_and_return_after_expiry() {
    let provider = super::super::Copilot::default();
    let first = credentials("first", "old-token");
    let renewed = credentials("first", "new-token");
    let other = credentials("second", "other-token");
    assert!(provider.model_visible(&entry(), "refused-model", &first));
    provider.observe_response_error(
        &first,
        "refused-model",
        400,
        br#"{"error":{"code":"model_not_supported"}}"#,
    );
    assert!(!provider.model_visible(&entry(), "refused-model", &renewed));
    assert!(provider.model_visible(&entry(), "refused-model", &other));
    let account = provider.refused.account(&first);
    let until = account.models.lock().unwrap()["refused-model"];
    assert!(until > Instant::now() + Duration::from_secs(6 * 3600 - 5));
    account.models.lock().unwrap().insert(
        "refused-model".into(),
        Instant::now() - Duration::from_secs(1),
    );
    assert!(provider.model_visible(&entry(), "refused-model", &renewed));
}

#[test]
fn wrong_endpoints_authentication_failures_and_api_key_errors_do_not_hide_a_model() {
    let provider = super::super::Copilot::default();
    let account = credentials("first", "token");
    for (status, body) in [
        (400, "unsupported_api_for_model"),
        (401, "requested model is not supported"),
        (429, "model_not_supported"),
    ] {
        provider.observe_response_error(&account, "refused-model", status, body.as_bytes());
    }
    assert!(provider.model_visible(&entry(), "refused-model", &account));
    let key = json!({"type":"apiKey", "access":"key"});
    provider.observe_response_error(
        &key,
        "refused-model",
        400,
        b"requested model is not supported",
    );
    assert!(provider.model_visible(&entry(), "refused-model", &key));
}

#[test]
fn refusal_account_identity_keeps_previous_sorted_keys() {
    let a: Value=serde_json::from_str(r#"{"accountIdentity":{"subject":"same","issuer":"service","scope":"org"}}"#).unwrap();
    let b: Value=serde_json::from_str(r#"{"accountIdentity":{"scope":"org","issuer":"service","subject":"same"}}"#).unwrap();
    assert_eq!(identity(&a),identity(&b));
    assert_eq!(identity(&a),"e4d755fbf42c6a342d6eea5991a0e837f924ce33ae6bf82012f07fa70c0dfdfe");
}
