use super::*;
use serde_json::json;

struct Cache(std::path::PathBuf);
impl Cache {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("nora-warmup-prompts-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        Self(root.join("models_cache.json"))
    }
    fn write(&self, value: &Value) {
        std::fs::write(&self.0, serde_json::to_vec(value).unwrap()).unwrap();
    }
}
impl Drop for Cache {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

#[test]
fn native_instructions_take_priority_then_cli_cache_then_codex_prompt() {
    let cache = Cache::new();
    cache.write(&json!({"models":[{"slug":"mini","base_instructions":"CLI base","model_messages":{"instructions_template":"CLI template"}}]}));
    let models = vec![
        json!({"id":"mini","base_instructions":"remote base","model_messages":{"instructions_template":"remote template"}}),
    ];
    assert_eq!(
        instructions("mini", &models, Some(&cache.0)),
        "remote template"
    );
    assert_eq!(instructions("mini", &[], Some(&cache.0)), "CLI template");
    cache.write(&json!({"models":[{"slug":"mini","base_instructions":"CLI base","model_messages":{"instructions_template":""}}]}));
    assert_eq!(instructions("mini", &[], Some(&cache.0)), "CLI base");
    assert_eq!(instructions("other", &models, Some(&cache.0)), FALLBACK);
    assert_eq!(instructions("mini", &[], None), FALLBACK);
    assert!(FALLBACK.starts_with("You are Codex,"));
}

#[test]
fn unreadable_incomplete_oversized_and_replaced_cache_never_erase_the_fallback() {
    let cache = Cache::new();
    assert_eq!(instructions("mini", &[], Some(&cache.0)), FALLBACK);
    for invalid in [
        "",
        "{",
        "null",
        "{\"models\":{}}",
        "{\"models\":[{\"slug\":\"mini\",\"base_instructions\":42}]}",
    ] {
        std::fs::write(&cache.0, invalid).unwrap();
        assert_eq!(instructions("mini", &[], Some(&cache.0)), FALLBACK);
    }
    std::fs::File::create(&cache.0)
        .unwrap()
        .set_len(MAX_CACHE_BYTES + 1)
        .unwrap();
    assert_eq!(instructions("mini", &[], Some(&cache.0)), FALLBACK);
    for text in ["first", "same-timestamp update"] {
        cache.write(&json!({"models":[{"slug":"mini","base_instructions":text}]}));
        assert_eq!(instructions("mini", &[], Some(&cache.0)), text);
    }
}
