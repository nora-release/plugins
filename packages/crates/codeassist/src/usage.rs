use super::*;
use nora_provider::usage::parsing::{number, quantities, result};
use std::collections::BTreeSet;

#[path = "usage/pools.rs"]
mod pools;
fn item(
    id: &str,
    label: &str,
    remaining: f64,
    reset: Option<&Value>,
    models: Vec<String>,
    span: Option<u64>,
    routing: bool,
) -> Option<Value> {
    if !remaining.is_finite() || !(0.0..=1.0).contains(&remaining) {
        return None;
    }
    let mut value = quantities(
        id,
        label,
        &json!({"used_percent":(1.0-remaining)*100.0}),
        None,
        reset,
    )?;
    value["countsForRouting"] = json!(routing);
    if !models.is_empty() {
        value["modelIDs"] = json!(models);
    }
    if let Some(span) = span {
        value["windowSeconds"] = json!(span);
    }
    Some(value)
}
pub(crate) fn gemini(payload: &Value, flags: &models::Flags) -> PluginResult<Value> {
    let buckets = payload["buckets"]
        .as_array()
        .ok_or_else(|| PluginError::transport("Gemini CLI did not return model quota buckets."))?;
    let mut seen = BTreeSet::new();
    let mut items = Vec::new();
    for bucket in buckets {
        let Some(raw) = text(bucket.get("modelId"), 256).filter(|id| !id.ends_with("_vertex"))
        else {
            continue;
        };
        let id = flags.shown(raw);
        if !seen.insert(id.clone()) {
            continue;
        }
        let Some(remaining) = bucket.get("remainingFraction").and_then(number) else {
            continue;
        };
        if let Some(value) = item(
            &id,
            &id,
            remaining,
            bucket.get("resetTime"),
            vec![id.clone()],
            None,
            true,
        ) {
            items.push(value);
        }
    }
    if items.is_empty() {
        return Err(PluginError::transport(
            "Gemini CLI returned no readable model allowances.",
        ));
    }
    Ok(result(&json!({}), items, Vec::new()))
}
pub(crate) fn antigravity(payload: &Value, summary: Option<&Value>) -> PluginResult<Value> {
    let models = payload["models"]
        .as_object()
        .ok_or_else(|| PluginError::transport("Antigravity returned no model allowances."))?;
    let mut items = Vec::new();
    let catalog = models::antigravity(payload).ok();
    for (id, model) in models {
        if !models::offered_row(id, model) {
            continue;
        }
        let Some(quota) = model.get("quotaInfo").filter(|quota| quota.is_object()) else {
            continue;
        };
        let remaining = match quota.get("remainingFraction") {
            None => Some(0.0),
            Some(value) => number(value),
        };
        let Some(remaining) = remaining else {
            continue;
        };
        let family = catalog.as_ref().and_then(|catalog| {
            catalog
                .variants
                .iter()
                .find(|(_, levels)| levels.values().any(|wire| wire == id))
        });
        let scope = family
            .map(|(family, levels)| {
                let mut scope = levels.values().cloned().collect::<BTreeSet<_>>();
                scope.insert(family.clone());
                scope.into_iter().collect()
            })
            .unwrap_or_else(|| vec![id.clone()]);
        if let Some(value) = item(
            id,
            text(model.get("displayName"), 512).unwrap_or(id),
            remaining,
            quota.get("resetTime"),
            scope,
            Some(18000),
            true,
        ) {
            let mut value = value;
            value["family"] = json!(vendor(
                id,
                text(model.get("displayName"), 512).unwrap_or(id)
            ));
            items.push(value);
        }
    }
    pools::append(&mut items, summary);
    if items.is_empty() {
        return Err(PluginError::transport(
            "Antigravity returned no readable allowances.",
        ));
    }
    Ok(result(&json!({}), items, Vec::new()))
}

fn vendor(id: &str, name: &str) -> String {
    let lower = id.to_ascii_lowercase();
    for (prefix, family) in [
        ("gemini", "Gemini"),
        ("claude", "Claude"),
        ("gpt-oss", "GPT-OSS"),
    ] {
        if lower.starts_with(prefix) {
            return family.into();
        }
    }
    name.split_whitespace()
        .next()
        .filter(|word| !word.is_empty())
        .unwrap_or_else(|| id.split('-').next().unwrap_or(id))
        .into()
}
#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
