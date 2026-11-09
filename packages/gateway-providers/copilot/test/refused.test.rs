use super::*;
use serde_json::json;

#[test]
fn a_refused_model_is_kept_for_its_account() {
    let dir = std::env::temp_dir().join(format!("copilot-refused-{}", random_hex(6)));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(FILE);
    let mut refused = Refused::load(Some(path.clone()));
    refused.add("octocat", "gpt-6-luna");
    assert!(refused.contains("octocat", "gpt-6-luna"));
    assert!(!refused.contains("hubot", "gpt-6-luna"));
    let again = Refused::load(Some(path));
    assert!(again.contains("octocat", "gpt-6-luna"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn only_copilots_model_refusal_counts() {
    assert!(refusal(
        400,
        &json!({"error": {"message": "The requested model is not supported.", "code": "model_not_supported"}})
    ));
    assert!(!refusal(
        400,
        &json!({"error": {"message": "bad", "code": "invalid"}})
    ));
    assert!(!refusal(
        500,
        &json!({"error": {"code": "model_not_supported"}})
    ));
}

#[test]
fn the_refusal_says_what_to_pick() {
    let error = explained("gpt-6-luna");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("gpt-6-luna") && message.contains("Auto"),
        "{message}"
    );
}
