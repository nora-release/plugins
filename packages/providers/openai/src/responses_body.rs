use serde_json::{json, Map, Value};
use std::{collections::BTreeMap, path::Path};
use url::Url;

#[path = "responses_input.rs"]
mod input;
#[path = "responses_prompts.rs"]
mod prompts;
#[path = "responses_search.rs"]
mod search;
#[path = "responses_session.rs"]
mod session;

const UNSUPPORTED: &[&str] = &[
    "max_output_tokens",
    "max_completion_tokens",
    "temperature",
    "top_p",
    "previous_response_id",
    "user",
    "safety_identifier",
    "prompt_cache_options",
];

pub(crate) fn initialize(directory: &Path) {
    prompts::initialize(directory);
}

pub(crate) fn remember_model(entry: &Value) {
    prompts::remember_model(entry);
}

pub(crate) fn instructions(model: &str) -> String {
    prompts::instructions(model)
}

pub(crate) fn finalize(
    subscription: bool,
    target: &Url,
    headers: &mut BTreeMap<String, String>,
    body: &mut Option<Vec<u8>>,
) {
    finalize_with_prompt(subscription, target, headers, body, prompts::instructions);
}

pub(super) fn finalize_with_prompt(
    subscription: bool,
    target: &Url,
    headers: &mut BTreeMap<String, String>,
    body: &mut Option<Vec<u8>>,
    instructions: impl FnOnce(&str) -> String,
) {
    if !subscription
        || target.scheme() != "https"
        || target.host_str() != Some("chatgpt.com")
        || target.path() != "/backend-api/codex/responses"
    {
        return;
    }
    let Some(bytes) = body.as_ref() else { return };
    let Ok(mut value) = serde_json::from_slice::<Value>(bytes) else {
        return;
    };
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let prompt = instructions(object.get("model").and_then(Value::as_str).unwrap_or(""));
    normalize(object, prompt);
    let Ok(bytes) = serde_json::to_vec(&value) else {
        return;
    };
    session::apply_headers(headers, &value);
    *body = Some(bytes);
}

fn normalize(object: &mut Map<String, Value>, mut prompt: String) {
    for key in UNSUPPORTED {
        object.remove(*key);
    }
    super::fast::apply(object);
    let raw_input = match object.remove("input") {
        Some(Value::String(text)) => vec![json!({
            "type":"message", "role":"user",
            "content":[{"type":"input_text", "text":text}]
        })],
        Some(Value::Array(input)) => input,
        _ => Vec::new(),
    };
    let mut input = input::normalize(raw_input);
    let own = object
        .get("instructions")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if object
        .get("prompt_cache_key")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        if let Some(key) = session::conversation_key(own, &input) {
            object.insert("prompt_cache_key".into(), json!(key));
        }
    }
    let own = object
        .get("instructions")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !own.trim().is_empty() {
        if first_line(own) == first_line(&prompt) {
            prompt = own.into();
        } else {
            input.insert(
                0,
                json!({
                    "type":"message", "role":"developer",
                    "content":[{"type":"input_text", "text":own}]
                }),
            );
        }
    }
    object.insert("input".into(), Value::Array(input));
    if !prompt.is_empty() {
        object.insert("instructions".into(), Value::String(prompt));
    }
    object.insert("store".into(), json!(false));
    object.insert("stream".into(), json!(true));
    object.entry("tool_choice").or_insert(json!("auto"));
    search::declare(object);
    object.entry("parallel_tool_calls").or_insert(json!(true));
    object
        .entry("text")
        .or_insert(json!({"verbosity":"medium"}));
    reasoning(object);
}

fn first_line(text: &str) -> &str {
    text.trim()
        .split_once('\n')
        .map_or(text.trim(), |(first, _)| first)
}

fn reasoning(object: &mut Map<String, Value>) {
    if let Some(reasoning) = object.get_mut("reasoning").and_then(Value::as_object_mut) {
        if reasoning.get("effort").and_then(Value::as_str) == Some("ultra") {
            reasoning.insert("effort".into(), json!("max"));
        }
        if reasoning.get("effort").and_then(Value::as_str) != Some("none") {
            reasoning.entry("summary").or_insert(json!("auto"));
        }
    }
    if object
        .get("reasoning")
        .and_then(|reasoning| reasoning.get("effort"))
        .and_then(Value::as_str)
        == Some("none")
    {
        object.remove("include");
        return;
    }
    let mut include = object
        .get("include")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !include.contains(&json!("reasoning.encrypted_content")) {
        include.push(json!("reasoning.encrypted_content"));
    }
    object.insert("include".into(), Value::Array(include));
}
