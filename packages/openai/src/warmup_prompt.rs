use serde_json::Value;
use std::{io::Read, path::Path};

const MAX_CACHE_BYTES: u64 = 8 * 1024 * 1024;
const FALLBACK: &str = include_str!("../assets/codex_prompt.txt");

fn prompt<'a>(models: &'a [Value], model: &str) -> Option<&'a str> {
    models.iter().rev().find_map(|entry| {
        let id = entry["slug"].as_str().or_else(|| entry["id"].as_str());
        if id != Some(model) {
            return None;
        }
        entry
            .pointer("/model_messages/instructions_template")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .or_else(|| {
                entry["base_instructions"]
                    .as_str()
                    .filter(|text| !text.is_empty())
            })
    })
}

pub(super) fn instructions(model: &str, models: &[Value], cli_cache: Option<&Path>) -> String {
    if let Some(text) = prompt(models, model) {
        return text.into();
    }
    let cached = cli_cache.and_then(|path| {
        let file = std::fs::File::open(path).ok()?;
        if file.metadata().ok()?.len() > MAX_CACHE_BYTES {
            return None;
        }
        let value: Value = serde_json::from_reader(file.take(MAX_CACHE_BYTES)).ok()?;
        prompt(value["models"].as_array()?, model).map(str::to_owned)
    });
    cached.unwrap_or_else(|| FALLBACK.into())
}

#[cfg(test)]
#[path = "../test/warmup_prompt.test.rs"]
mod tests;
