use serde_json::{json, Value};
use std::collections::BTreeMap;
use url::Url;

const BETA: &str = "fast-mode-2026-02-01";

fn supported(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    let id = id
        .rsplit_once('-')
        .filter(|(_, date)| date.len() == 8 && date.bytes().all(|byte| byte.is_ascii_digit()))
        .map_or(id.as_str(), |(model, _)| model);
    matches!(id, "claude-opus-4-8" | "claude-opus-5" | "claude-opus-5-5")
}

pub(crate) fn models(result: &mut Value, subscription: bool) {
    let default_api = result["defaultAPI"].as_str().unwrap_or_default().to_owned();
    let default_base = result["defaultBaseURL"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let Some(models) = result["models"].as_array_mut() else {
        return;
    };
    for model in models {
        let api = model["api"].as_str().unwrap_or(&default_api);
        let base = model["baseUrl"].as_str().unwrap_or(&default_base);
        let official = Url::parse(base)
            .ok()
            .is_some_and(|url| url.host_str() == Some("api.anthropic.com"));
        let fast = !subscription
            && official
            && api == "anthropic-messages"
            && supported(model["id"].as_str().unwrap_or_default());
        if !model["info"].is_object() {
            model["info"] = json!({});
        }
        model["info"]["serviceTiers"] = if fast {
            json!(["auto", "priority"])
        } else {
            json!([])
        };
    }
}

pub(crate) fn finalize(
    target: &Url,
    headers: &mut BTreeMap<String, String>,
    body: &mut Option<Vec<u8>>,
) {
    if target.host_str() != Some("api.anthropic.com") || !target.path().ends_with("/messages") {
        return;
    }
    let Some(bytes) = body.as_ref() else { return };
    let Ok(mut value) = serde_json::from_slice::<Value>(bytes) else {
        return;
    };
    if !supported(value["model"].as_str().unwrap_or_default()) {
        return;
    }
    let fast_tier = matches!(value["service_tier"].as_str(), Some("fast" | "priority"));
    if !fast_tier && value["speed"] != "fast" {
        return;
    }
    if fast_tier {
        value.as_object_mut().unwrap().remove("service_tier");
        value["speed"] = json!("fast");
        if let Ok(bytes) = serde_json::to_vec(&value) {
            *body = Some(bytes);
        }
    }
    let mut betas: Vec<String> = Vec::new();
    for (_, value) in headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("anthropic-beta"))
    {
        for beta in value
            .split(',')
            .map(str::trim)
            .filter(|beta| !beta.is_empty())
        {
            if !betas.iter().any(|existing| existing == beta) {
                betas.push(beta.into());
            }
        }
    }
    if !betas.iter().any(|beta| beta == BETA) {
        betas.push(BETA.into());
    }
    headers.retain(|name, _| !name.eq_ignore_ascii_case("anthropic-beta"));
    headers.insert("anthropic-beta".into(), betas.join(","));
}

#[cfg(test)]
#[path = "../test/fast.test.rs"]
mod tests;
