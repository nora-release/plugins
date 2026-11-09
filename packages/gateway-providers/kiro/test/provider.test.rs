use super::*;

#[test]
fn an_account_shows_its_label_and_renews_before_its_access_expires() {
    let saved = crate::fixture::saved();
    let credential = Credential::from_saved(&saved).unwrap();
    let account = account(&credential);
    assert_eq!(account.name, credential.label());
    assert_eq!(account.credentials["refresh"], "private-refresh");
    assert!(account
        .expires_at
        .as_deref()
        .is_some_and(|at| at.ends_with('Z')));
    let key = Credential::api_key(&json!({"key": "ksk_private"})).unwrap();
    assert_eq!(expires_at(&key), None);
}

#[tokio::test]
async fn an_unknown_sign_in_session_must_start_again() {
    let kiro = Kiro::default();
    let finish = SignInFinish {
        question: Question {
            provider: "kiro".into(),
            ..Question::default()
        },
        session: "unknown".into(),
        code: String::new(),
    };
    assert_eq!(
        kiro.sign_in_finish(finish).await.unwrap_err().code,
        nora_plugin_runtime_sdk::provider::SIGN_IN_NEEDED
    );
}

#[test]
fn a_request_outside_kiros_messages_endpoint_is_refused_on_its_address() {
    let kiro = Kiro::default();
    assert!(kiro
        .check_address("POST", "https://evil.example/v1/messages")
        .is_err());
    assert!(kiro
        .check_address("GET", "https://runtime.us-east-1.kiro.dev/v1/messages")
        .is_err());
    assert!(kiro
        .check_address(
            "POST",
            "https://runtime.us-east-1.kiro.dev/v1/messages?debug=1"
        )
        .is_err());
    assert!(kiro
        .check_address(
            "POST",
            "https://runtime.us-east-1.kiro.dev/v1/messages?beta=true"
        )
        .is_ok());
}
