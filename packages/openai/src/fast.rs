use serde_json::{json, Value};
use url::Url;

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
        let host = Url::parse(base)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned));
        let id = model["id"].as_str().unwrap_or_default();
        let chat = model.pointer("/info/chat") != Some(&Value::Bool(false))
            && model.pointer("/info/imageOutput") != Some(&Value::Bool(true))
            && model.pointer("/info/embedding") != Some(&Value::Bool(true));
        let supported = chat
            && if subscription {
                api == "chatgpt-codex"
                    && host.as_deref() == Some("chatgpt.com")
                    && id.starts_with("gpt-")
            } else {
                api == "openai-responses"
                    && host.as_deref() == Some("api.openai.com")
                    && (id.starts_with("gpt-")
                        || id.strip_prefix('o').is_some_and(|rest| {
                            rest.as_bytes().first().is_some_and(u8::is_ascii_digit)
                        }))
            };
        let mut tiers: Vec<String> = model
            .pointer("/info/serviceTiers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|tier| {
                if subscription {
                    supported && matches!(*tier, "auto" | "priority" | "ultrafast")
                } else {
                    !matches!(*tier, "fast" | "ultrafast") && (supported || *tier != "priority")
                }
            })
            .map(str::to_owned)
            .collect();
        if supported {
            for tier in ["auto", "priority"] {
                if !tiers.iter().any(|existing| existing == tier) {
                    tiers.push(tier.into());
                }
            }
        }
        if !model["info"].is_object() {
            model["info"] = json!({});
        }
        model["info"]["serviceTiers"] = json!(tiers);
    }
}

pub(crate) fn finalize(subscription: bool, target: &Url, body: &mut Option<Vec<u8>>) {
    if !subscription
        || target.host_str() != Some("chatgpt.com")
        || !target.path().ends_with("/responses")
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
    if !object.contains_key("service_tier")
        || matches!(
            object["service_tier"].as_str(),
            Some("priority" | "ultrafast")
        )
    {
        return;
    }
    object.remove("service_tier");
    if let Ok(bytes) = serde_json::to_vec(&value) {
        *body = Some(bytes);
    }
}

#[cfg(test)]
#[path = "../test/fast.test.rs"]
mod tests;
