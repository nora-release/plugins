use fs2::FileExt;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

#[path = "warmup_prompt.rs"]
mod fallback;

const MAX_CACHE_BYTES: u64 = 8 * 1024 * 1024;

fn cache_path() -> &'static Mutex<Option<PathBuf>> {
    static CACHE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

pub(super) fn initialize(directory: &Path) {
    if let Ok(mut path) = cache_path().lock() {
        *path = Some(directory.join("codex-model-prompts.json"));
    }
}

pub(super) fn remember_model(entry: &Value) {
    let path = cache_path().lock().ok().and_then(|path| path.clone());
    if let Some(path) = path {
        let _ = remember_at(&path, entry);
    }
}

pub(super) fn instructions(model: &str) -> String {
    let saved = cache_path().lock().ok().and_then(|path| path.clone());
    instructions_at(
        saved.as_deref(),
        model,
        super::super::codex_version::models_cache_path().as_deref(),
    )
}

fn instructions_at(saved: Option<&Path>, model: &str, cli_cache: Option<&Path>) -> String {
    if let Some(prompt) = saved.and_then(|path| read(path).remove(model)) {
        return prompt;
    }
    fallback::instructions(model, &[], cli_cache)
}

fn read(path: &Path) -> BTreeMap<String, String> {
    let read = || {
        let file = File::open(path).ok()?;
        if file.metadata().ok()?.len() > MAX_CACHE_BYTES {
            return None;
        }
        serde_json::from_reader(file.take(MAX_CACHE_BYTES)).ok()
    };
    read().unwrap_or_default()
}

fn remember_at(path: &Path, entry: &Value) -> std::io::Result<()> {
    let id = entry["slug"].as_str().or_else(|| entry["id"].as_str());
    let prompt = entry
        .pointer("/model_messages/instructions_template")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .or_else(|| {
            entry["base_instructions"]
                .as_str()
                .filter(|text| !text.is_empty())
        });
    let (Some(id), Some(prompt)) = (id, prompt) else {
        return Ok(());
    };
    let Some(directory) = path.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(directory)?;
    let mut options = private_options();
    let lock = options
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("codex-model-prompts.lock"))?;
    lock.lock_exclusive()?;
    let mut prompts = read(path);
    if prompts.get(id).is_some_and(|previous| previous == prompt) {
        return Ok(());
    }
    prompts.insert(id.into(), prompt.into());
    let bytes = serde_json::to_vec(&prompts)?;
    if bytes.len() as u64 > MAX_CACHE_BYTES {
        return Ok(());
    }
    let temporary = directory.join(format!(".codex-model-prompts-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = private_options()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        File::open(directory)?.sync_all()
    })();
    let _ = std::fs::remove_file(temporary);
    result
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

#[cfg(test)]
#[path = "../test/responses_prompts.test.rs"]
mod tests;
