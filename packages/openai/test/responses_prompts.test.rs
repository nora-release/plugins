use super::*;
use serde_json::json;

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("nora-response-prompts-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn saved_model_prompts_override_cli_cache_and_preserve_other_accounts_models() {
    let directory = Directory::new();
    let saved = directory.0.join("prompts.json");
    let cli = directory.0.join("models_cache.json");
    std::fs::write(&cli, serde_json::to_vec(&json!({"models":[
        {"slug":"model", "base_instructions":"CLI base", "model_messages":{"instructions_template":"CLI template"}}
    ]})).unwrap()).unwrap();
    assert_eq!(
        instructions_at(Some(&saved), "model", Some(&cli)),
        "CLI template"
    );
    remember_at(&saved, &json!({"slug":"model", "base_instructions":"remote base", "model_messages":{"instructions_template":"remote template"}})).unwrap();
    remember_at(
        &saved,
        &json!({"id":"another-account-model", "base_instructions":"second account"}),
    )
    .unwrap();
    assert_eq!(
        instructions_at(Some(&saved), "model", Some(&cli)),
        "remote template"
    );
    assert_eq!(
        instructions_at(Some(&saved), "another-account-model", None),
        "second account"
    );
    remember_at(
        &saved,
        &json!({"slug":"model", "base_instructions":"new prompt"}),
    )
    .unwrap();
    assert_eq!(
        instructions_at(Some(&saved), "model", Some(&cli)),
        "new prompt"
    );
    assert_eq!(
        instructions_at(Some(&saved), "another-account-model", None),
        "second account"
    );
    assert!(instructions_at(Some(&saved), "missing", Some(&cli)).starts_with("You are Codex,"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            saved.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn prompt_saves_from_concurrent_workers_keep_each_models_latest_complete_prompt() {
    let directory = Directory::new();
    let saved = directory.0.join("prompts.json");
    let workers = (0..12).map(|index| {
        let path = saved.clone();
        std::thread::spawn(move || {
            remember_at(&path, &json!({"slug":format!("model-{index}"), "base_instructions":format!("prompt-{index}")})).unwrap();
        })
    }).collect::<Vec<_>>();
    for worker in workers {
        worker.join().unwrap();
    }
    let prompts = read(&saved);
    assert_eq!(prompts.len(), 12);
    for index in 0..12 {
        assert_eq!(
            prompts[&format!("model-{index}")],
            format!("prompt-{index}")
        );
    }
    assert!(!std::fs::read_dir(&directory.0).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with('.')));
}

#[test]
fn bad_or_oversized_saved_prompts_fall_back_without_destroying_cli_instructions() {
    let directory = Directory::new();
    let saved = directory.0.join("prompts.json");
    let cli = directory.0.join("models_cache.json");
    std::fs::write(
        &cli,
        br#"{"models":[{"slug":"model","base_instructions":"CLI base"}]}"#,
    )
    .unwrap();
    for malformed in ["{", "null", r#"{"model":42}"#] {
        std::fs::write(&saved, malformed).unwrap();
        assert_eq!(
            instructions_at(Some(&saved), "model", Some(&cli)),
            "CLI base"
        );
    }
    File::create(&saved)
        .unwrap()
        .set_len(MAX_CACHE_BYTES + 1)
        .unwrap();
    assert_eq!(
        instructions_at(Some(&saved), "model", Some(&cli)),
        "CLI base"
    );
}
