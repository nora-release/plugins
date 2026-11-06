use super::{
    account,
    models::{self, Wire},
    network, prompt, schema,
};
use nora_provider_plugin_sdk::{FetchRequest, PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::BTreeSet;
pub(crate) const FAST_BETA: &str = "fast-mode-2026-02-01";
pub(crate) struct Request {
    pub wire: Wire,
    pub path: String,
    pub query: String,
    pub model: Value,
    pub body: Value,
    pub names: BTreeSet<String>,
    pub fast: bool,
}
fn invalid() -> PluginError {
    PluginError::invalid("This request is outside Factory's declared native protocol endpoints.")
}
fn numbers(value: &Value) -> PluginResult<()> {
    match value {
        Value::Number(n) if n.is_f64() => {
            let f = n.as_f64().unwrap();
            if !f.is_finite() || f.fract() == 0. && f.abs() > 9007199254740991. {
                return Err(PluginError::invalid(
                    "This Factory request contains a number that cannot be preserved exactly.",
                ));
            }
        }
        Value::Array(v) => {
            for v in v {
                numbers(v)?;
            }
        }
        Value::Object(v) => {
            for v in v.values() {
                numbers(v)?;
            }
        }
        _ => {}
    }
    Ok(())
}
pub(crate) fn parse(request: &FetchRequest) -> PluginResult<Request> {
    let url = url::Url::parse(&request.url).map_err(|_| invalid())?;
    if request.method != "POST"
        || url.origin() != url::Url::parse(network::API).unwrap().origin()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    let path = url.path();
    let (wire, model, path) = match path {
        "/api/llm/a/v1/messages" | "/api/llm/a/v1/messages/count_tokens" => {
            (Wire::Anthropic, None, path.to_owned())
        }
        "/api/llm/o/v1/responses" => (Wire::Responses, None, path.to_owned()),
        "/api/llm/o/v1/chat/completions" => (Wire::Chat, None, path.to_owned()),
        _ => {
            let tail = path
                .strip_prefix("/api/llm/g/v1/models/")
                .ok_or_else(invalid)?;
            let (model, operation) = tail.rsplit_once(':').ok_or_else(invalid)?;
            if !["streamGenerateContent", "generateContent"].contains(&operation)
                || model.is_empty()
                || model
                    .bytes()
                    .any(|b| !b.is_ascii_alphanumeric() && !b"._-".contains(&b))
            {
                return Err(invalid());
            }
            (
                Wire::Gemini,
                Some(model.to_owned()),
                "/api/llm/g/v1/generate".into(),
            )
        }
    };
    if !url.query_pairs().all(|(k, v)| match wire {
        Wire::Anthropic => k == "beta" && v == "true",
        Wire::Gemini => k == "key" || k == "alt" && v == "sse",
        _ => false,
    }) {
        return Err(invalid());
    }
    if request
        .body_file
        .as_ref()
        .is_some_and(|f| f.length > 64 * 1024 * 1024)
    {
        return Err(PluginError::invalid(
            "The Factory JSON body exceeds 64 MiB.",
        ));
    }
    let bytes = request.decode_body()?.ok_or_else(invalid)?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err(PluginError::invalid(
            "The Factory JSON body exceeds 64 MiB.",
        ));
    }
    let mut body: Value = serde_json::from_slice(&bytes)
        .map_err(|_| PluginError::invalid("The Factory request body is invalid JSON."))?;
    if !body.is_object() {
        return Err(invalid());
    }
    numbers(&body)?;
    let mut model = model
        .or_else(|| account::text(body.get("model"), 256).map(str::to_owned))
        .ok_or_else(invalid)?;
    if wire == Wire::Responses
        && body
            .get("instructions")
            .is_some_and(|v| !v.is_null() && !v.is_string())
    {
        return Err(PluginError::invalid(
            "Factory Responses instructions must be text.",
        ));
    }
    let fast_asked = wire == Wire::Anthropic && body["speed"] == "fast"
        || wire == Wire::Responses && body["service_tier"] == "priority";
    if fast_asked && models::find(&format!("{model}-fast")).is_some() {
        model = format!("{model}-fast");
    }
    let row = models::find(&model)
        .ok_or_else(|| PluginError::invalid("This model is not in Factory's Droid registry."))?;
    if models::wire(&row) != Some(wire) {
        return Err(PluginError::invalid(
            "This Factory model requires its own native protocol.",
        ));
    }
    let fast = model.ends_with("-fast") && matches!(wire, Wire::Anthropic | Wire::Responses);
    body["model"] = json!(model);
    if fast {
        body[if wire == Wire::Anthropic {
            "speed"
        } else {
            "service_tier"
        }] = json!(if wire == Wire::Anthropic {
            "fast"
        } else {
            "priority"
        });
    }
    if row["upstream"] == "openai"
        && (body.get("temperature").is_some() || body.get("top_p").is_some())
    {
        return Err(PluginError::invalid(
            "Factory's OpenAI reasoning models do not support temperature or top_p.",
        ));
    }
    prompt::adapt(&mut body, wire);
    let names = if wire == Wire::Anthropic {
        schema::wrap(&mut body)?
    } else {
        BTreeSet::new()
    };
    Ok(Request {
        wire,
        path,
        query: if wire == Wire::Anthropic && url.query().is_some() {
            "?beta=true".into()
        } else {
            String::new()
        },
        model: row,
        body,
        names,
        fast: fast && wire == Wire::Anthropic,
    })
}
#[cfg(test)]
#[path = "../test/request.test.rs"]
mod tests;
