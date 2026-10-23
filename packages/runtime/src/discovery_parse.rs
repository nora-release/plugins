use super::Kind;
use crate::{definition::Definition, models};
use nora_ai::{InputModality, Model};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::Value;
use std::collections::BTreeMap;

pub fn entries(payload: &Value, kind: Kind) -> PluginResult<&[Value]> {
    if payload.get("success").and_then(Value::as_bool) == Some(false) {
        return Err(PluginError::transport(
            "The provider could not list its models.",
        ));
    }
    let rows = match kind {
        Kind::Google => payload.get("models"),
        _ => payload
            .get("data")
            .filter(|value| value.as_array().is_some_and(|values| !values.is_empty()))
            .or_else(|| payload.get("models"))
            .or_else(|| payload.get("data")),
    };
    rows.or_else(|| payload.is_array().then_some(payload))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| PluginError::transport("The provider returned an invalid model list."))
}

pub fn model(
    entry: &Value,
    kind: Kind,
    definition: &Definition,
    credentials: &Value,
    catalog: &BTreeMap<String, Model>,
) -> Option<Model> {
    let id = definition
        .adapter
        .model_id(entry, credentials)
        .or_else(|| match kind {
            Kind::Google => text(entry, &["/name"]),
            _ => entry.as_str().or_else(|| text(entry, &["/id", "/name"])),
        })?;
    let id = if kind == Kind::Google {
        id.strip_prefix("models/").unwrap_or(id)
    } else {
        id
    };
    let id = id.trim();
    if id.is_empty()
        || id.len() > 512
        || id.chars().any(char::is_control)
        || entry.get("active").and_then(Value::as_bool) == Some(false)
        || !definition.adapter.model_visible(entry, id, credentials)
    {
        return None;
    }
    let mut model = catalog.get(id).cloned().unwrap_or_else(|| {
        let mut model = Model::new(id, &definition.api, &definition.id);
        model.context_window = 0;
        model.max_tokens = 0;
        model
    });
    models::configure_model(definition, Some(credentials), &mut model);
    if let Some(name) = text(
        entry,
        &["/magpie_label", "/display_name", "/displayName", "/name"],
    )
    .filter(|name| *name != id && !name.starts_with("models/") && !name.contains(": "))
    {
        model.name = name.into();
    }
    if let Some(context) = number(
        entry,
        &[
            "/context_window",
            "/context_length",
            "/inputTokenLimit",
            "/capabilities/limits/max_context_window_tokens",
            "/capabilities/limits/max_prompt_tokens",
        ],
    ) {
        model.context_window = context;
    }
    if let Some(output) = number(
        entry,
        &[
            "/max_output_tokens",
            "/outputTokenLimit",
            "/max_tokens",
            "/top_provider/max_completion_tokens",
            "/capabilities/limits/max_output_tokens",
        ],
    ) {
        model.max_tokens = output;
    }
    if let Some(input) = entry
        .get("input_modalities")
        .or_else(|| entry.pointer("/modalities/input"))
        .or_else(|| entry.pointer("/architecture/input_modalities"))
        .and_then(Value::as_array)
    {
        model.input = input
            .iter()
            .filter_map(|value| serde_json::from_value::<InputModality>(value.clone()).ok())
            .collect();
    } else if let Some(vision) = boolean(
        entry,
        &[
            "/supportsImageInput",
            "/capabilities/supports/vision",
            "/capabilities/vision",
            "/vision",
        ],
    ) {
        model.input = vec![InputModality::Text];
        if vision {
            model.input.push(InputModality::Image);
        }
    }
    let efforts = entry
        .get("supported_reasoning_levels")
        .or_else(|| entry.get("reasoning_efforts"))
        .or_else(|| entry.pointer("/capabilities/supports/reasoning_effort"))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| {
                    value
                        .as_str()
                        .or_else(|| text(value, &["/effort", "/value"]))
                })
                .filter(|effort| !effort.is_empty() && effort.len() < 64)
                .map(str::to_string)
                .collect::<Vec<_>>()
        });
    if let Some(efforts) = efforts {
        model.reasoning = !efforts.is_empty();
        model.info.reasoning_efforts = efforts;
    } else if let Some(reasoning) = boolean(
        entry,
        &[
            "/reasoning",
            "/thinking",
            "/capabilities/reasoning",
            "/capabilities/supports/reasoning",
            "/capabilities/supports/thinking",
        ],
    ) {
        model.reasoning = reasoning;
    }
    if let Some(context) =
        number(entry, &["/max_context_window"]).filter(|context| *context > model.context_window)
    {
        model.info.max_context_window = Some(context);
    }
    if let Some(tiers) = entry.get("service_tiers").and_then(Value::as_array) {
        let tiers = tiers
            .iter()
            .filter_map(|tier| tier.as_str().or_else(|| text(tier, &["/id"])))
            .map(str::to_string)
            .collect::<Vec<_>>();
        if !tiers.is_empty() {
            model.info.service_tiers = tiers;
        }
    }
    let parameters = entry.get("supported_parameters").and_then(Value::as_array);
    let tool_calling = boolean(
        entry,
        &[
            "/supportsTools",
            "/capabilities/supports/tool_calls",
            "/capabilities/function_calling",
            "/capabilities/completion_tools",
            "/tool_calling",
        ],
    )
    .or_else(|| {
        parameters.map(|values| values.iter().any(|value| value.as_str() == Some("tools")))
    });
    if let Some(tool_calling) = tool_calling {
        model.info.tool_calling = Some(tool_calling);
    }
    definition
        .adapter
        .enrich_model(entry, &mut model, credentials);
    crate::pricing::apply(entry, &mut model);
    let media = models::apply_media_metadata(entry, &mut model);
    if !media && model.info.chat != Some(true) && !is_chat(entry, kind, id) {
        return None;
    }
    model.normalize_info();
    Some(model)
}

fn is_chat(entry: &Value, kind: Kind, id: &str) -> bool {
    if entry.get("active").and_then(Value::as_bool) == Some(false) {
        return false;
    }
    if kind == Kind::Google {
        return entry
            .get("supportedGenerationMethods")
            .and_then(Value::as_array)
            .is_none_or(|methods| {
                methods.is_empty()
                    || methods
                        .iter()
                        .any(|method| method.as_str() == Some("generateContent"))
            });
    }
    if text(entry, &["/capabilities/type"]).is_some_and(|kind| kind != "chat") {
        return false;
    }
    if entry
        .pointer("/capabilities/completion_chat")
        .and_then(Value::as_bool)
        == Some(false)
    {
        return false;
    }
    if let Some(output) = entry
        .pointer("/modalities/output")
        .or_else(|| entry.pointer("/architecture/output_modalities"))
        .and_then(Value::as_array)
    {
        if !output.iter().any(|value| value.as_str() == Some("text")) {
            return false;
        }
    }
    let id = id.to_ascii_lowercase();
    ![
        "embed",
        "-tts",
        "image",
        "audio",
        "-live",
        "robotics",
        "computer-use",
        "deep-research",
        "transcribe",
        "realtime",
        "moderation",
        "whisper",
        "dall-e",
        "sora",
    ]
    .iter()
    .any(|bad| id.contains(bad))
}

fn text<'a>(entry: &'a Value, paths: &[&str]) -> Option<&'a str> {
    paths.iter().find_map(|path| {
        entry
            .pointer(path)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })
}
fn number(entry: &Value, paths: &[&str]) -> Option<u64> {
    paths.iter().find_map(|path| {
        entry
            .pointer(path)
            .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
            .filter(|value| *value > 0)
    })
}
fn boolean(entry: &Value, paths: &[&str]) -> Option<bool> {
    paths
        .iter()
        .find_map(|path| entry.pointer(path).and_then(Value::as_bool))
}
