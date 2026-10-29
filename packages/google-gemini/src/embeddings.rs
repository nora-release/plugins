use crate::provider::{json_response, native_request, send_json, JsonResponse};
use base64::{engine::general_purpose::STANDARD, Engine};
use nora_ai::{HttpClient, HttpRequest};
use nora_provider::media_upload::MediaBody;
use nora_provider_plugin_sdk::{FetchResponse, PluginError, PluginResult};
use serde_json::{json, Value};

pub async fn fetch(
    template: HttpRequest,
    body: MediaBody,
    client: &dyn HttpClient,
) -> PluginResult<FetchResponse> {
    let MediaBody::Json(value) = body else {
        return Err(PluginError::invalid(
            "Google embeddings require a JSON request.",
        ));
    };
    let model = value
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| PluginError::invalid("Name the Google embedding model."))?;
    let (texts, batch) = match value.get("input") {
        Some(Value::String(text)) => (vec![text.as_str()], false),
        Some(Value::Array(texts)) if !texts.is_empty() && texts.len() <= 100 => (
            texts
                .iter()
                .map(|text| {
                    text.as_str().ok_or_else(|| {
                        PluginError::invalid(
                            "Google embedding inputs must be strings, not token IDs.",
                        )
                    })
                })
                .collect::<PluginResult<Vec<_>>>()?,
            true,
        ),
        _ => {
            return Err(PluginError::invalid(
                "Supply a string or between 1 and 100 strings for Google embeddings.",
            ))
        }
    };
    let format = value
        .get("encoding_format")
        .and_then(Value::as_str)
        .unwrap_or("float");
    if !matches!(format, "float" | "base64") {
        return Err(PluginError::invalid(
            "Embedding encoding_format must be float or base64.",
        ));
    }
    let mut config = json!({});
    if let Some(dimensions) = value.get("dimensions") {
        let dimensions = dimensions
            .as_u64()
            .filter(|dimensions| *dimensions > 0 && *dimensions <= 65536)
            .ok_or_else(|| {
                PluginError::invalid(
                    "Embedding dimensions must be a positive integer no greater than 65536.",
                )
            })?;
        config["outputDimensionality"] = json!(dimensions);
    }
    for (source, target) in [("task_type", "taskType"), ("title", "title")] {
        if let Some(item) = value.get(source) {
            if !item.is_string() {
                return Err(PluginError::invalid(
                    "Google embedding task_type and title must be strings.",
                ));
            }
            config[target] = item.clone();
        }
    }
    let resource = format!("models/{}", model.strip_prefix("models/").unwrap_or(model));
    let requests = texts
        .iter()
        .map(|text| {
            let mut request = json!({"model":resource,"content":{"parts":[{"text":text}]}});
            if config.as_object().is_some_and(|config| !config.is_empty()) {
                request["embedContentConfig"] = config.clone();
            }
            request
        })
        .collect::<Vec<_>>();
    let native = if batch {
        json!({"requests":requests})
    } else {
        requests[0].clone()
    };
    let response = send_json(
        client,
        native_request(
            template,
            model,
            if batch {
                "batchEmbedContents"
            } else {
                "embedContent"
            },
            native,
        )?,
    )
    .await?;
    let (answer, headers) = match response {
        JsonResponse::Error(error) => return Ok(error),
        JsonResponse::Value(answer, headers) => (answer, headers),
    };
    let embeddings = if batch {
        answer
            .get("embeddings")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(invalid_embedding)?
    } else {
        vec![answer
            .get("embedding")
            .cloned()
            .ok_or_else(invalid_embedding)?]
    };
    if embeddings.len() != texts.len() {
        return Err(invalid_embedding());
    }
    let mut data = Vec::with_capacity(embeddings.len());
    let mut dimensions = config
        .get("outputDimensionality")
        .and_then(Value::as_u64)
        .map(|count| count as usize);
    for (index, embedding) in embeddings.iter().enumerate() {
        let values = embedding
            .get("values")
            .and_then(Value::as_array)
            .filter(|values| !values.is_empty())
            .ok_or_else(invalid_embedding)?;
        if dimensions.is_some_and(|dimensions| dimensions != values.len()) {
            return Err(invalid_embedding());
        }
        dimensions = Some(values.len());
        let output = if format == "base64" {
            let mut bytes = Vec::with_capacity(values.len() * 4);
            for value in values {
                let value = value
                    .as_f64()
                    .filter(|value| value.is_finite() && (*value as f32).is_finite())
                    .ok_or_else(invalid_embedding)?;
                bytes.extend_from_slice(&(value as f32).to_le_bytes());
            }
            json!(STANDARD.encode(bytes))
        } else {
            if values
                .iter()
                .any(|value| value.as_f64().is_none_or(|value| !value.is_finite()))
            {
                return Err(invalid_embedding());
            }
            json!(values)
        };
        data.push(json!({"object":"embedding","index":index,"embedding":output}));
    }
    let mut result = json!({"object":"list","model":model,"data":data});
    if let Some(tokens) = answer
        .pointer("/usageMetadata/promptTokenCount")
        .or_else(|| answer.pointer("/usage_metadata/prompt_token_count"))
        .and_then(Value::as_u64)
    {
        result["usage"] = json!({"prompt_tokens":tokens,"total_tokens":tokens});
    }
    json_response(headers, result)
}

fn invalid_embedding() -> PluginError {
    PluginError::transport("Google returned an invalid or incomplete embedding response.")
}

#[cfg(test)]
#[path = "../test/embeddings.test.rs"]
mod tests;
