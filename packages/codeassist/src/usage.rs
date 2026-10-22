use super::*;
use nora_provider::usage::parsing::{number, quantities, result};
use std::collections::BTreeSet;
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
        if !models::valid_id(id) || !models::visible(id) {
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
                .map(|(family, _)| family.clone())
        });
        let scope = family
            .filter(|family| family != id)
            .map(|family| vec![id.clone(), family])
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
            items.push(value);
        }
    }
    if let Some(groups) = summary.and_then(|value| value["groups"].as_array()) {
        for (index, group) in groups.iter().enumerate() {
            let name = text(group.get("displayName"), 512).unwrap_or("Shared allowance");
            for (bucket_index, bucket) in group["buckets"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                if bucket["disabled"] == true {
                    continue;
                }
                let span = match bucket["window"].as_str() {
                    Some("5h") => Some(18000),
                    Some("weekly") => Some(604800),
                    _ => None,
                };
                let remaining = match bucket.get("remainingFraction") {
                    None => Some(0.0),
                    Some(value) => number(value),
                };
                let Some(remaining) = remaining else {
                    continue;
                };
                let label = format!(
                    "{name} · {}",
                    text(
                        bucket.get("displayName").or_else(|| bucket.get("window")),
                        128
                    )
                    .unwrap_or("Allowance")
                );
                if let Some(value) = item(
                    &format!("pool:{index}:{bucket_index}"),
                    &label,
                    remaining,
                    bucket.get("resetTime"),
                    Vec::new(),
                    span,
                    false,
                ) {
                    items.push(value);
                }
            }
        }
    }
    if items.is_empty() {
        return Err(PluginError::transport(
            "Antigravity returned no readable allowances.",
        ));
    }
    Ok(result(&json!({}), items, Vec::new()))
}
#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
