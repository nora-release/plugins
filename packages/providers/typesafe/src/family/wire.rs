use super::*;

pub(super) const CF_JEV: &str = "typesafe/jev";
pub(super) const CF_CLEF: &str = "@cf/cloudflare/clef";
pub(super) const CF_FLASH: &str = "@cf/cloudflare/clef-flash";

pub(super) fn clef(model: &str) -> bool {
    matches!(model, CF_CLEF | CF_FLASH)
}

pub(super) fn ask(
    family: DecisionFamily,
    base: &str,
    bytes: Vec<u8>,
) -> PluginResult<(String, Vec<u8>, BTreeMap<String, String>)> {
    let mut value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| PluginError::invalid("A decision body must contain JSON."))?;
    let model = core::required(&value, "model")?.to_owned();
    let mut headers = BTreeMap::new();
    match family {
        DecisionFamily::Cloudflare => {
            value.as_object_mut().unwrap().remove("model");
            if clef(&model) {
                if value["questions"]
                    .as_object()
                    .is_some_and(|questions| questions.len() > 64)
                {
                    return Err(PluginError::invalid(
                        "Cloudflare Clef accepts at most 64 questions per request.",
                    ));
                }
                value["model"] = json!(model.trim_start_matches("@cf/cloudflare/"));
                Ok((
                    endpoints::append(base, &model)?,
                    serde_json::to_vec(&value).unwrap(),
                    headers,
                ))
            } else if model == CF_JEV {
                Ok((
                    base.into(),
                    serde_json::to_vec(&json!({"model":model,"input":value})).unwrap(),
                    headers,
                ))
            } else {
                Err(PluginError::invalid(
                    "This model has no supported Cloudflare decision invocation.",
                ))
            }
        }
        DecisionFamily::Vercel if endpoints::legacy(base) => {
            value.as_object_mut().unwrap().remove("model");
            if let Some(questions) = value["questions"].as_object_mut() {
                for question in questions.values_mut() {
                    if question["type"] == "noul" {
                        question["type"] = json!("boolean");
                    }
                }
            }
            headers.insert("ai-model-id".into(), model);
            headers.insert(
                "ai-evaluation-model-specification-version".into(),
                "4".into(),
            );
            Ok((
                endpoints::append(base, "evaluation-model")?,
                serde_json::to_vec(&value).unwrap(),
                headers,
            ))
        }
        _ => Ok((endpoints::append(base, "systemone")?, bytes, headers)),
    }
}

fn confidence(probabilities: &serde_json::Map<String, Value>) -> f64 {
    let values = probabilities
        .values()
        .filter_map(Value::as_f64)
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    if values.len() < 2 {
        return 1.0;
    }
    let top = values.into_iter().fold(0.0f64, f64::max);
    let floor = 1.0 / probabilities.values().filter_map(Value::as_f64).count() as f64;
    ((top - floor) / (1.0 - floor)).clamp(0.0, 1.0)
}

pub(super) fn answer(family: DecisionFamily, base: &str, bytes: &[u8]) -> PluginResult<Vec<u8>> {
    let mut value: Value = serde_json::from_slice(bytes)
        .map_err(|_| PluginError::transport("The decision provider returned invalid JSON."))?;
    if family == DecisionFamily::Cloudflare {
        if value["success"] == false {
            return Err(PluginError::transport(
                "Cloudflare refused the decision request.",
            ));
        }
        if value["result"].is_object() {
            value = value["result"].take();
        }
    }
    if family == DecisionFamily::Vercel && endpoints::legacy(base) {
        if let Some(answers) = value["answers"].as_object_mut() {
            for answer in answers.values_mut() {
                if answer["type"] == "boolean" {
                    answer["type"] = json!("noul");
                    answer["noul"] = answer["probability"].clone();
                } else if answer.get("confidence").is_none() {
                    if let Some(probabilities) = answer["probabilities"].as_object() {
                        answer["confidence"] = json!(confidence(probabilities));
                    }
                }
            }
        }
        if let Some(usage) = value["usage"].as_object_mut() {
            for (old, new) in [
                ("inputTokens", "input_tokens"),
                ("outputTokens", "output_tokens"),
            ] {
                if let Some(tokens) = usage.remove(old) {
                    usage.insert(new.into(), tokens);
                }
            }
        }
    }
    if family == DecisionFamily::Vercel && value.pointer("/usage/cost").is_none() {
        if let Some(cost) = value
            .pointer("/provider_metadata/gateway/cost")
            .and_then(|value| {
                value
                    .as_f64()
                    .or_else(|| value.as_str()?.parse::<f64>().ok())
            })
            .filter(|cost| cost.is_finite() && *cost >= 0.0)
        {
            if value["usage"].is_object() {
                value["usage"]["cost"] = json!({"total":cost});
            }
        }
    }
    if value
        .get("answers")
        .and_then(Value::as_object)
        .is_none_or(|answers| answers.is_empty())
    {
        return Err(PluginError::transport(
            "The decision provider returned no SystemOne answers.",
        ));
    }
    Ok(serde_json::to_vec(&value).unwrap())
}

#[cfg(test)]
#[path = "../../test/wire.test.rs"]
mod tests;
