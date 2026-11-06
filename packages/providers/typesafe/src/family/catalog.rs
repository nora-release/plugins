use super::*;
use std::collections::BTreeSet;

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|number| number.is_finite() && *number >= 0.0)
}

pub(super) fn models(
    entries: &[Value],
    definition: &Definition,
    base: &str,
    cloudflare: bool,
) -> PluginResult<Vec<Value>> {
    if entries.len() > 4096 {
        return Err(PluginError::transport(
            "The decision provider returned too many models.",
        ));
    }
    let mut seen = BTreeSet::new();
    let mut models = Vec::new();
    for entry in entries {
        let id = entry
            .as_str()
            .or_else(|| {
                cloudflare
                    .then(|| entry.get("name").and_then(Value::as_str))
                    .flatten()
            })
            .or_else(|| entry.get("id").and_then(Value::as_str))
            .or_else(|| entry.get("name").and_then(Value::as_str))
            .map(str::trim)
            .filter(|id| !id.is_empty() && id.len() <= 1024 && !id.chars().any(char::is_control))
            .ok_or_else(|| {
                PluginError::transport("The decision provider returned an invalid model ID.")
            })?;
        if cloudflare && !matches!(id, wire::CF_JEV | wire::CF_CLEF | wire::CF_FLASH) {
            continue;
        }
        if !seen.insert(id.to_owned()) {
            continue;
        }
        let preset = definition.models.iter().find(|model| model["id"] == id);
        let mut model = preset.cloned().unwrap_or_else(|| json!({"id":id}));
        model["name"] = json!(entry
            .get("display_name")
            .or_else(|| entry.get("displayName"))
            .or_else(|| entry.get("name"))
            .and_then(Value::as_str)
            .or_else(|| preset.and_then(|model| model["name"].as_str()))
            .unwrap_or(id));
        for (target, keys) in [
            (
                "contextWindow",
                &[
                    "contextWindow",
                    "context_length",
                    "context_window",
                    "max_context_window",
                ][..],
            ),
            ("maxTokens", &["max_output_tokens", "max_tokens"][..]),
        ] {
            if let Some(value) = keys
                .iter()
                .find_map(|key| entry.get(*key).and_then(Value::as_u64))
            {
                model[target] = json!(value);
            }
        }
        if let Some(input) = entry
            .get("input")
            .or_else(|| entry.pointer("/modalities/input"))
            .or_else(|| entry.pointer("/architecture/input_modalities"))
            .or_else(|| entry.get("input_modalities"))
            .and_then(Value::as_array)
        {
            model["input"] = json!(input
                .iter()
                .filter_map(Value::as_str)
                .filter(|value| matches!(*value, "text" | "image" | "audio" | "video" | "pdf"))
                .collect::<Vec<_>>());
        }
        let mut cost = serde_json::Map::new();
        for (target, keys) in [
            ("input", &["input"][..]),
            ("output", &["output"][..]),
            ("cacheRead", &["input_cache_read", "cache_read"][..]),
            ("cacheWrite", &["input_cache_write", "cache_write"][..]),
        ] {
            let actual = entry
                .get("cost")
                .and_then(|cost| cost.get(target))
                .and_then(number);
            let per_token = keys.iter().find_map(|key| {
                entry
                    .get("pricing")
                    .and_then(|pricing| pricing.get(*key))
                    .and_then(number)
            });
            if let Some(value) = actual
                .or_else(|| per_token.map(|value| value * 1_000_000.0))
                .filter(|value| value.is_finite())
            {
                cost.insert(target.into(), json!(value));
            }
        }
        if !cost.is_empty() {
            model["cost"] = Value::Object(cost.clone());
            model["info"]["pricingKnown"] = json!(true);
            if let (Some(input), Some(output)) = (
                cost.get("input").and_then(number),
                cost.get("output").and_then(number),
            ) {
                model["info"]["free"] = json!(input == 0.0 && output == 0.0);
            }
        }
        if let Some(free) = entry.get("free").and_then(Value::as_bool) {
            model["info"]["free"] = json!(free);
        }
        models.push(crate::provider::marked(model, definition, base, "remote"));
    }
    Ok(models)
}

pub(super) fn result(models: Vec<Value>, base: &str, source: &str) -> Value {
    json!({"models":models,"source":source,"allowUnknownModels":false,"defaultAPI":"systemone","defaultBaseURL":base})
}

#[cfg(test)]
#[path = "../../test/catalog.test.rs"]
mod tests;
