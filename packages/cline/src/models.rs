use super::provider::{headers, BASE};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use nora_provider_subscription_core as core;
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Default)]
pub(crate) struct Catalog {
    pub models: Vec<Value>,
    pub images: BTreeMap<String, bool>,
}

const INPUT_KINDS: [&str; 5] = ["text", "image", "audio", "video", "pdf"];

fn inputs(entry: &Value) -> Option<[bool; 5]> {
    let modalities = entry
        .pointer("/architecture/input_modalities")
        .filter(|value| !value.is_null())
        .or_else(|| entry.get("input_modalities"))?
        .as_array()
        .filter(|values| !values.is_empty())?;
    Some(INPUT_KINDS.map(|kind| {
        modalities.iter().any(|value| {
            value
                .as_str()
                .is_some_and(|value| value.eq_ignore_ascii_case(kind))
        })
    }))
}
fn id(entry: &Value) -> Option<&str> {
    entry.as_str().or_else(|| {
        ["id", "model", "modelId", "model_id", "key", "slug"]
            .iter()
            .find_map(|key| core::text(&entry[key]))
    })
}
fn valid_id(id: &str) -> bool {
    id.len() <= 2048
        && id.split('/').count() == 2
        && id.split('/').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        })
}
fn name(id: &str, entry: &Value) -> String {
    ["display_name", "displayName", "name", "label"]
        .iter()
        .find_map(|key| core::text(&entry[key]))
        .filter(|name| *name != id && Some(*name) != id.rsplit('/').next())
        .unwrap_or(id)
        .to_owned()
}
fn number(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|value| value.parse::<u64>().ok()))
        .filter(|n| *n > 0)
}
fn bare(id: &str) -> String {
    id.rsplit('/')
        .next()
        .unwrap_or(id)
        .split(':')
        .next()
        .unwrap_or(id)
        .to_ascii_lowercase()
}
pub(crate) fn parse(recommended: &Value, cloud: &Value, source: &str) -> PluginResult<Catalog> {
    let recommended = recommended.get("data").unwrap_or(recommended);
    let mut order: Vec<String> = Vec::new();
    let mut facts: BTreeMap<String, Value> = BTreeMap::new();
    for group in ["recommended", "free", "clinePass"] {
        for entry in recommended[group].as_array().into_iter().flatten() {
            let Some(id) = id(entry).filter(|id| valid_id(id)) else {
                continue;
            };
            let free = group == "free" || entry["free"] == true || entry["free"] == "true";
            let pass = group == "clinePass" || id.starts_with("cline-pass/");
            let own = facts.entry(id.into()).or_insert_with(|| {
                order.push(id.into());
                json!({"name":name(id,entry),"free":false,"pass":false})
            });
            if free {
                own["free"] = json!(true);
                own["name"] = json!(name(id, entry));
            }
            if pass {
                own["pass"] = json!(true);
            }
        }
    }
    let cloud = cloud
        .as_array()
        .or_else(|| cloud["data"].as_array())
        .cloned()
        .unwrap_or_default();
    if cloud.len() > core::MAX_MODELS || order.len() > core::MAX_MODELS {
        return Err(PluginError::transport("Cline returned too many models."));
    }
    for entry in &cloud {
        let Some(id) = core::text(&entry["id"])
            .filter(|id| id.len() <= 2048 && !id.chars().any(char::is_control))
        else {
            continue;
        };
        if !facts.contains_key(id) {
            order.push(id.into());
            facts.insert(
                id.into(),
                json!({"name":name(id,entry),"free":false,"pass":id.starts_with("cline-pass/")}),
            );
        }
    }
    let mut output = Catalog::default();
    for id in order {
        let own = &facts[&id];
        let item = cloud.iter().find(|item| item["id"] == id);
        let context = item
            .and_then(|item| {
                number(&item["context_length"])
                    .or_else(|| number(&item["top_provider"]["context_length"]))
                    .or_else(|| number(&item["context"]))
            })
            .unwrap_or(0);
        let input = if let Some(item) = item {
            inputs(item)
        } else {
            let mut matches = cloud
                .iter()
                .filter(|entry| {
                    entry["id"]
                        .as_str()
                        .is_some_and(|other| bare(other) == bare(&id))
                })
                .filter_map(inputs);
            matches.next().filter(|first| matches.all(|m| m == *first))
        };
        let images = input.is_some_and(|input| input[1]);
        if input.is_some() {
            output.images.insert(id.clone(), images);
        }
        let title = format!(
            "{}{}",
            own["name"].as_str().unwrap_or(&id),
            if own["free"] == true {
                " (free)"
            } else if own["pass"] == true {
                " (ClinePass)"
            } else {
                ""
            }
        );
        let mut model = core::model(
            &id,
            &title,
            "openai-completions",
            "cline",
            BASE,
            context,
            0,
            images,
            false,
            Vec::new(),
            source,
        );
        model["info"]["free"] = own["free"].clone();
        output.models.push(model);
    }
    if output.models.is_empty() {
        return Err(PluginError::transport("Cline returned no valid models."));
    }
    Ok(output)
}
pub(crate) async fn discover(client: &dyn nora_ai::HttpClient) -> PluginResult<Catalog> {
    let request = |path: &str| {
        let mut request = core::json_request(
            nora_ai::HttpMethod::Get,
            format!("{BASE}{path}"),
            None,
            None,
        )?;
        request.headers.extend(headers(None));
        Ok::<_, PluginError>(request)
    };
    let recommended = core::checked(client, request("/ai/cline/recommended-models")?).await?;
    let cloud = core::checked(client, request("/ai/cline/models")?)
        .await
        .unwrap_or(Value::Null);
    parse(&recommended, &cloud, "remote")
}
pub(crate) fn presets() -> Catalog {
    let rows: Vec<Value> = serde_json::from_str(include_str!("models.json")).unwrap_or_default();
    let mut groups = json!({"recommended":[],"free":[]});
    let mut cloud = Vec::new();
    for row in rows {
        if row["input"].is_object() {
            cloud.push(json!({
                "id": row["id"],
                "input_modalities": INPUT_KINDS.iter().filter(|kind| row["input"][**kind] == true).collect::<Vec<_>>()
            }));
        }
        let target = if row["free"] == true {
            "free"
        } else {
            "recommended"
        };
        groups[target].as_array_mut().unwrap().push(row);
    }
    parse(&groups, &json!(cloud), "preset").unwrap_or_default()
}
#[cfg(test)]
#[path = "../test/models.test.rs"]
mod tests;
