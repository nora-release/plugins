use super::*;

#[test]
fn a_pick_of_models_session_names_its_model() {
    let read = pick(&json!({
        "session_token": "st", "selected_model": "mai-code-1.1-flash",
        "available_models": ["mai-code-1.1-flash"], "expires_at": 4_000_000_000u64
    }))
    .unwrap();
    assert_eq!(read.model, "mai-code-1.1-flash");
    assert_eq!(read.token, "st");
    assert!(read.usable());
}

#[test]
fn a_pick_of_auto_names_the_model_it_sends() {
    let read = pick(&json!({"session_token": "st", "selected_model": {"id": "gpt-6-luna", "supported_endpoints": ["/responses"]}})).unwrap();
    assert_eq!(read.model, "gpt-6-luna");
    let first = pick(&json!({"session_token": "st", "available_models": ["a", "b"]})).unwrap();
    assert_eq!(first.model, "a");
    assert!(pick(&json!({"selected_model": "a"})).is_none());
}

#[test]
fn a_pick_about_to_expire_is_asked_again() {
    let read =
        pick(&json!({"session_token": "st", "selected_model": "a", "expires_at": 1u64})).unwrap();
    assert!(!read.usable());
}

#[test]
fn copilot_turning_a_session_away_is_told_apart() {
    assert!(session_refused(
        400,
        &json!({"error": {"message": "Requested model not available for session", "code": ""}})
    ));
    assert!(!session_refused(
        400,
        &json!({"error": {"message": "The requested model is not supported.", "code": "model_not_supported"}})
    ));
    assert!(!session_refused(
        500,
        &json!({"error": {"message": "session"}})
    ));
}
