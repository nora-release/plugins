use crate::definition::Definition;
use nora_ai::{InputModality, JsonValue, Model, ModelsCatalog};
use serde_json::Value;

pub fn catalog_models_with_catalog(
    definition: &Definition,
    credentials: Option<&Value>,
    catalog: &ModelsCatalog,
) -> Result<Vec<Model>, String> {
    let default_catalog = if definition.config.catalog.is_empty() {
        &definition.id
    } else {
        &definition.config.catalog
    };
    let catalog_id = definition.adapter.catalog(credentials, default_catalog);
    let mut models = Vec::new();
    for catalog_id in catalog_id
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        for model in catalog.models_for(catalog_id) {
            if !models
                .iter()
                .any(|existing: &Model| existing.id == model.id)
            {
                models.push(model.clone());
            }
        }
    }
    if models.is_empty() {
        models = catalog
            .models_for(&definition.id)
            .into_iter()
            .cloned()
            .collect();
    }
    for value in definition
        .config
        .model_definitions
        .as_ref()
        .unwrap_or(&definition.models)
    {
        let Some(id) = value
            .as_str()
            .or_else(|| value.get("id").and_then(Value::as_str))
        else {
            continue;
        };
        if models.iter().any(|model| model.id == id) {
            continue;
        }
        let mut model = catalog.model(catalog_id, id).cloned().unwrap_or_else(|| {
            let mut model = Model::new(id, &definition.api, &definition.id);
            model.context_window = 0;
            model.max_tokens = 0;
            model
        });
        if let Some(fields) = value.as_object() {
            let mut normalized = serde_json::to_value(&model).map_err(|error| error.to_string())?;
            normalized
                .as_object_mut()
                .ok_or("Invalid model profile")?
                .extend(fields.clone());
            model = serde_json::from_value(normalized).map_err(|error| error.to_string())?;
            crate::pricing::apply(value, &mut model);
        }
        if model.name.is_empty() {
            model.name = id.into();
        }
        models.push(model);
    }
    for model in &mut models {
        model.normalize_info();
        crate::pricing::apply(&Value::Null, model);
        apply_media_metadata(&Value::Null, model);
    }
    models.sort_by(|left, right| {
        right
            .info
            .release_date
            .cmp(&left.info.release_date)
            .then_with(|| left.id.cmp(&right.id))
    });
    for model in &mut models {
        configure_model(definition, credentials, model);
    }
    models.retain(|model| {
        definition.adapter.retain_model(credentials, model)
            && crate::profiles::retain_model(definition, model)
    });
    Ok(models)
}

/// Keep media-only models in the account catalog without treating them as chat.
pub fn apply_media_metadata(entry: &Value, model: &mut Model) -> bool {
    let id = model.id.to_ascii_lowercase();
    let kind = entry
        .get("kind")
        .or_else(|| entry.pointer("/capabilities/type"))
        .or_else(|| entry.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let outputs = entry
        .pointer("/modalities/output")
        .or_else(|| entry.pointer("/architecture/output_modalities"))
        .or_else(|| entry.get("output_modalities"))
        .and_then(Value::as_array);
    let output = |name: &str| {
        outputs.is_some_and(|values| values.iter().any(|value| value.as_str() == Some(name)))
    };
    let image = model.info.image_output.unwrap_or_else(|| {
        output("image")
            || matches!(kind, "image" | "image_generation")
            || model.api.contains("image")
            || [
                "gpt-image",
                "chatgpt-image",
                "dall-e",
                "imagen-",
                "flux-",
                "stable-diffusion",
                "seedream",
                "grok-imagine-image",
            ]
            .iter()
            .any(|part| id.contains(part))
            || (id.starts_with("gemini-") && id.contains("-image"))
    });
    let video = model.info.video_output.unwrap_or_else(|| {
        output("video")
            || kind == "video"
            || model.api.contains("video")
            || ["grok-imagine-video", "sora-", "veo-", "seedance"]
                .iter()
                .any(|part| id.contains(part))
    });
    let embedding = model.info.embedding.unwrap_or_else(|| {
        matches!(kind, "embedding" | "embeddings")
            || model.api.contains("embedding")
            || id.contains("embedding")
            || id.contains("embed-")
    });
    let other = matches!(kind, "audio" | "rerank" | "transcription" | "speech")
        || id.contains("rerank")
        || id.contains("whisper")
        || id.contains("-tts")
        || id.contains("transcribe");
    let media = image || video || embedding || other;
    if image {
        model.info.image_output = Some(true);
    }
    if video {
        model.info.video_output = Some(true);
    }
    if embedding {
        model.info.embedding = Some(true);
    }
    if media {
        let generates_content = entry
            .get("supportedGenerationMethods")
            .and_then(Value::as_array)
            .is_some_and(|methods| {
                methods
                    .iter()
                    .any(|method| method.as_str() == Some("generateContent"))
            });
        model.info.chat = model.info.chat.or(Some(
            outputs.map_or(generates_content && !video && !embedding, |_| {
                output("text")
            }),
        ));
        if model.info.chat == Some(false) {
            model.info.tool_calling = model.info.tool_calling.or(Some(false));
        }
    }
    media
}

pub fn configure_model(definition: &Definition, credentials: Option<&Value>, model: &mut Model) {
    let endpoint = effective_endpoint(definition, credentials);
    model.provider = definition.id.clone();
    if model.input.is_empty() {
        model.input = vec![InputModality::Text];
    }
    model.base_url = endpoint.clone();
    model.api = definition
        .adapter
        .model_api(credentials, &model.api)
        .unwrap_or_else(|| definition.api.clone());
    if !definition.config.headers.is_empty() {
        model
            .headers
            .get_or_insert_with(Default::default)
            .extend(definition.config.headers.clone());
    }
    model
        .query_params
        .extend(definition.config.query_params.clone());
    model.provider_options.extend(
        definition
            .config
            .provider_options
            .iter()
            .map(|(key, value)| (key.clone(), JsonValue::from(value.clone()))),
    );
    if definition.config.endpoint_kind.as_deref() == Some("region") {
        model
            .provider_options
            .insert("region".into(), JsonValue::string(&definition.base_url));
    }
    model.normalize_info();
    crate::profiles::route_model(definition, model);
}

pub fn effective_endpoint(definition: &Definition, credentials: Option<&Value>) -> String {
    if let Some(endpoint) = definition.adapter.endpoint(credentials) {
        return endpoint;
    }
    if definition.config.endpoint_kind.as_deref() == Some("region") {
        return format!(
            "https://bedrock-runtime.{}.amazonaws.com",
            definition.base_url
        );
    }
    definition.base_url.clone()
}

#[cfg(test)]
#[path = "../test/models.test.rs"]
mod tests;
