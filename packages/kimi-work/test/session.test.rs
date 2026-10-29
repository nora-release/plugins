use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "nora-kimi-work-source-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn document(user: &str, token: &str) -> Value {
    json!({"credentials":{"kimiCode":{"baseUrl":super::super::BASE,"apiKey":token},"kimiWeb":{"userId":user,"accessToken":"synthetic-membership"}},"model":{"providers":{"daimon-kimi-code":{"baseUrl":"http://localhost:2345/v1"}}}})
}

#[test]
fn selected_sources_remain_read_only_and_bind_identity_while_tokens_rotate() {
    let directory = Directory::new();
    let path = directory.0.join("config.json");
    let original = serde_json::to_vec(&document("synthetic-user", "synthetic-work")).unwrap();
    std::fs::write(&path, &original).unwrap();
    let session = read_selected(&path).unwrap();
    let account = credentials(&session, Some(path.to_str().unwrap()));
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(!account.to_string().contains("synthetic-work"));
    assert!(!account.to_string().contains("synthetic-membership"));
    std::fs::write(
        &path,
        serde_json::to_vec(&document("synthetic-user", "rotated-work")).unwrap(),
    )
    .unwrap();
    assert_eq!(current(Some(&account)).unwrap().work_token, "rotated-work");
    std::fs::write(
        &path,
        serde_json::to_vec(&document("different-user", "different-work")).unwrap(),
    )
    .unwrap();
    assert!(current(Some(&account)).is_err());
}

#[test]
fn missing_explicit_selection_foreign_work_endpoints_and_missing_tokens_are_rejected() {
    assert!(read_selected(Path::new("config.json")).is_err());
    assert!(current(None).is_err());
    let directory = Directory::new();
    let path = directory.0.join("config.json");
    let mut value = document("synthetic-user", "synthetic-work");
    value["credentials"]["kimiCode"]["baseUrl"] = json!("https://api.kimi.com/coding/v1");
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(read_selected(&path).is_err());
    for input in [
        json!({"workToken":"synthetic-work"}),
        json!({"workToken":"synthetic-work","membershipToken":"invalid\r\nheader"}),
    ] {
        assert!(pasted(&input).is_err());
    }
}
