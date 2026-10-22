use super::*;
use std::path::PathBuf;
struct File(PathBuf);
impl File {
    fn new(payload: Value) -> Self {
        let path = std::env::temp_dir().join(format!(
            "nora-google-source-{}-{}.json",
            std::process::id(),
            nora_ai::oauth_pkce::random_hex(8)
        ));
        std::fs::write(&path, serde_json::to_vec(&payload).unwrap()).unwrap();
        Self(path)
    }
}
impl Drop for File {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
#[test]
fn an_explicit_source_is_read_only_and_its_saved_reference_contains_no_tokens() {
    let file = File::new(
        json!({"refresh_token":"private-refresh","access_token":"private-access","expiry_date":now()+3600000,"client_id":App::GeminiCLI.client_id()}),
    );
    let before = std::fs::read(&file.0).unwrap();
    let mut tokens = selected(file.0.to_str().unwrap(), App::GeminiCLI).unwrap();
    tokens.uid = "bound-account".into();
    tokens.email = "person@example.test".into();
    let saved = saved(&tokens, App::GeminiCLI, Some(file.0.to_str().unwrap()));
    assert!(!saved.to_string().contains("private-"));
    assert_eq!(std::fs::read(&file.0).unwrap(), before);
    assert!(current(Some(&saved), App::Antigravity).is_err());
}
#[test]
fn source_rotation_is_read_each_time_and_foreign_oauth_clients_are_refused() {
    let file = File::new(
        json!({"token":{"refresh_token":"first","client_id":App::Antigravity.client_id()}}),
    );
    let first = selected(file.0.to_str().unwrap(), App::Antigravity).unwrap();
    assert_eq!(first.refresh, "first");
    std::fs::write(&file.0, br#"{"token":{"refresh_token":"second"}}"#).unwrap();
    assert_eq!(
        selected(file.0.to_str().unwrap(), App::Antigravity)
            .unwrap()
            .refresh,
        "second"
    );
    assert!(selected("relative.json", App::GeminiCLI).is_err());
    let foreign =
        File::new(json!({"refresh_token":"private","client_id":App::Antigravity.client_id()}));
    assert!(selected(foreign.0.to_str().unwrap(), App::GeminiCLI).is_err());
}
#[test]
fn export_lists_and_malformed_sources_are_not_imported() {
    let list = File::new(json!([{"refresh_token":"first"},{"refresh_token":"second"}]));
    assert!(selected(list.0.to_str().unwrap(), App::Antigravity).is_err());
    let wrong = File::new(json!({"type":"gemini","refresh_token":"private"}));
    assert!(selected(wrong.0.to_str().unwrap(), App::Antigravity).is_err());
}
