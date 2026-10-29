use crate::*;
use nora_ai::{InputModality, Model};
pub(crate) fn fallback(provider: &JoyCode) -> PluginResult<Vec<Model>> {
    Ok(
        provider.config()?.manifest["contributes"]["provider"]["models"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|entry| {
                let mut model = Model::new(text(entry, "id"), "openai-completions", "joycode");
                model.base_url = format!("{BASE}/v1");
                model.name = text(entry, "name").into();
                model.context_window = entry["contextWindow"].as_u64().unwrap_or(200000);
                model.max_tokens = entry["maxTokens"].as_u64().unwrap_or(64000);
                model.reasoning = entry["reasoning"] == true;
                model.input = if entry["input"]
                    .as_array()
                    .is_some_and(|items| items.contains(&json!("image")))
                {
                    vec![InputModality::Text, InputModality::Image]
                } else {
                    vec![InputModality::Text]
                };
                model.info.chat = Some(true);
                model.info.tool_calling = Some(true);
                model
            })
            .collect(),
    )
}
pub(crate) async fn load(provider: &JoyCode, context: CallContext) -> PluginResult<Value> {
    let Some(credentials) = context.credentials else {
        return Ok(
            json!({"models":fallback(provider)?,"source":"preset","allowUnknownModels":false}),
        );
    };
    ticket(&credentials)?;
    let client = provider.client(context.network_proxy.as_ref())?;
    let (list, rates) = tokio::join!(
        query(
            client.as_ref(),
            &credentials,
            "joycode_modelList",
            json!({})
        ),
        query(
            client.as_ref(),
            &credentials,
            "plugin_config",
            json!({"sceneType":"billingRate"})
        )
    );
    for result in [&list, &rates] {
        if let Err(error) = result {
            if error.code == "authentication_required" {
                return Err(error.clone());
            }
        }
    }
    let Ok(list) = list else {
        return Ok(
            json!({"models":fallback(provider)?,"source":"preset","allowUnknownModels":false}),
        );
    };
    if !list.is_array() {
        return Ok(
            json!({"models":fallback(provider)?,"source":"preset","allowUnknownModels":false}),
        );
    }
    let models = parse(
        &list,
        rates.as_ref().ok(),
        meta(&credentials, "tenant"),
        &fallback(provider)?,
    );
    Ok(
        json!({"models":models,"source":"remote","allowUnknownModels":false,"defaultAPI":"openai-completions","defaultBaseURL":format!("{BASE}/v1")}),
    )
}
pub(crate) fn multiplier(value: &Value) -> Option<f64> {
    if let Some(value) = value.as_f64() {
        return (value.is_finite() && value >= 0.0).then_some(value);
    }
    let text = value.as_str()?.trim();
    let number = text.strip_suffix(['x', 'X', '×']).unwrap_or(text).trim();
    if number.is_empty()
        || !number
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
        || number.matches('.').count() > 1
        || number.starts_with('.')
        || number.ends_with('.')
    {
        return None;
    }
    number
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
}
fn billing(value: &Value, depth: u8) -> Option<&Value> {
    if depth > 5 {
        return None;
    }
    if value["billingRate"].is_object() {
        return value.get("billingRate");
    }
    if value["billingRates"].is_array() {
        return Some(value);
    }
    for key in ["data", "result", "config", "content", "value"] {
        if let Some(value) = value.get(key).and_then(|value| billing(value, depth + 1)) {
            return Some(value);
        }
    }
    None
}
pub(crate) fn parse(
    list: &Value,
    rates: Option<&Value>,
    tenant: &str,
    fallback: &[Model],
) -> Vec<Model> {
    let scene = rates.and_then(|value| billing(value, 0));
    let tenant = tenant.trim();
    let excluded = scene
        .and_then(|scene| scene["excludeTenants"].as_array())
        .is_some_and(|tenants| {
            tenants
                .iter()
                .filter_map(Value::as_str)
                .any(|name| name.trim() == tenant)
        });
    let rates = if !tenant.is_empty() && !excluded {
        scene
            .and_then(|scene| scene["billingRates"].as_array())
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                Some((
                    text(entry, "chatApiModel"),
                    multiplier(&entry["billingRate"])?,
                ))
            })
            .collect::<BTreeMap<_, _>>()
    } else {
        BTreeMap::new()
    };
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let id = text(entry, "chatApiModel");
            if !valid(id, 512)
                || entry["hidden"] == true
                || entry["isHidden"] == true
                || !matches!(
                    text(entry, "modelFunctionType"),
                    "" | "ALL" | "chat" | "agent"
                )
            {
                return None;
            }
            let mut model = fallback
                .iter()
                .find(|model| model.id == id)
                .cloned()
                .unwrap_or_else(|| Model::new(id, "openai-completions", "joycode"));
            model.base_url = format!("{BASE}/v1");
            model.name = if text(entry, "label").is_empty() {
                id
            } else {
                text(entry, "label")
            }
            .into();
            model.context_window =
                entry["maxTotalTokens"]
                    .as_u64()
                    .unwrap_or(if model.context_window > 0 {
                        model.context_window
                    } else {
                        200000
                    });
            model.max_tokens = entry["respMaxTokens"]
                .as_u64()
                .unwrap_or(if model.max_tokens > 0 {
                    model.max_tokens
                } else {
                    64000
                });
            let features = entry["features"].as_array();
            let has =
                |feature: &str| features.is_some_and(|features| features.contains(&json!(feature)));
            model.input = if has("vision") {
                vec![InputModality::Text, InputModality::Image]
            } else {
                vec![InputModality::Text]
            };
            model.info.chat = Some(true);
            model.info.tool_calling = Some(has("function_call"));
            model.info.free = None;
            model.info.credit_multiplier = None;
            model.info.previous_credit_multiplier = None;
            if let Some(rate) = multiplier(&entry["billingRate"]).or_else(|| rates.get(id).copied())
            {
                model.info.credit_multiplier = serde_json::Number::from_f64(rate);
                model.info.free = Some(rate == 0.0);
            }
            Some(model)
        })
        .collect()
}
pub(crate) fn flatten(body: &Value) -> Value {
    let mut systems = Vec::new();
    let mut turns = Vec::new();
    for message in body["messages"].as_array().into_iter().flatten() {
        let content = match message.get("content") {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Array(parts)) => parts
                .iter()
                .map(|part| {
                    if let Some(text) = part.as_str() {
                        text
                    } else {
                        match text(part, "type") {
                            "text" => text(part, "text"),
                            "image_url" => "[image omitted]",
                            _ => "",
                        }
                    }
                })
                .collect(),
            _ => String::new(),
        };
        let content = content.trim();
        if content.is_empty() {
            continue;
        }
        match text(message, "role") {
            "system" | "developer" => systems.push(content.to_owned()),
            "user" => turns.push(format!("User: {content}")),
            "assistant" => turns.push(format!("Assistant: {content}")),
            "tool" => turns.push(format!("Tool result: {content}")),
            _ => {}
        }
    }
    let prompt = if turns.len() == 1 && turns[0].starts_with("User: ") {
        turns[0][6..].into()
    } else if turns.is_empty() {
        systems.join("\n\n")
    } else {
        format!("{}\n\nAssistant:", turns.join("\n\n"))
    };
    let mut messages = Vec::new();
    if !systems.is_empty() {
        messages.push(json!({"role":"system","content":systems.join("\n\n")}));
    }
    messages.push(json!({"role":"user","content":prompt}));
    let mut result =
        json!({"model":body["model"],"messages":messages,"stream":body["stream"]==true});
    if result["stream"] != true {
        result["max_tokens"] = json!(64000);
    }
    result
}
