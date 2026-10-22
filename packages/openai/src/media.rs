use base64::Engine;
use nora_ai::HttpClient;
use nora_provider::{definition::Definition, media_upload::MediaBody, transport};
use nora_provider_plugin_sdk::{FetchRequest, FetchResponse};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use url::Url;

pub async fn fetch(
    definition: &Definition,
    request: &FetchRequest,
    client: &dyn HttpClient,
) -> PluginResult<Option<FetchResponse>> {
    let oauth = nora_provider::adapter::is_oauth(request.credentials.as_ref());
    if !oauth
        || !Url::parse(&request.url).is_ok_and(|url| url.path().ends_with("/images/edits"))
        || !request.headers.iter().any(|(key, value)| {
            key.eq_ignore_ascii_case("content-type")
                && value
                    .to_ascii_lowercase()
                    .starts_with("multipart/form-data")
        })
    {
        return Ok(None);
    }
    let MediaBody::Multipart(form) = nora_provider::media_upload::parse(request).await? else {
        return Ok(None);
    };
    let mut value = serde_json::Map::new();
    for (key, text) in form.fields {
        let field = if key == "n" {
            json!(text.parse::<u32>().ok().filter(|n| *n > 0).ok_or_else(|| {
                PluginError::invalid("The image count must be a positive whole number.")
            })?)
        } else {
            json!(text)
        };
        value.insert(key, field);
    }
    let mut images = Vec::new();
    for file in form.files {
        if !file.mime_type.starts_with("image/") {
            return Err(PluginError::invalid(
                "Image edits only accept image uploads.",
            ));
        }
        match file.field.as_str() {
            "image" | "image[]" | "images" | "images[]" => {
                images.push(json!({"image_url":file.data_url()}))
            }
            "mask" if !value.contains_key("mask") => {
                value.insert("mask".into(), json!({"image_url":file.data_url()}));
            }
            _ => {
                return Err(PluginError::invalid(
                    "The image edit upload contains an unsupported or duplicate file field.",
                ))
            }
        }
    }
    if images.is_empty() {
        return Err(PluginError::invalid(
            "An image edit needs at least one image.",
        ));
    }
    value.insert("images".into(), json!(images));
    let mut request = request.clone();
    request.body_file = None;
    request.body =
        Some(base64::engine::general_purpose::STANDARD.encode(
            serde_json::to_vec(&value).map_err(|_| {
                PluginError::invalid("The image edit request could not be encoded.")
            })?,
        ));
    request.headers.retain(|key, _| {
        !key.eq_ignore_ascii_case("content-type") && !key.eq_ignore_ascii_case("content-length")
    });
    request
        .headers
        .insert("content-type".into(), "application/json".into());
    let outgoing = transport::build_request(definition, request).await?;
    let secrets = transport::request_secrets(&outgoing);
    let response = client
        .stream(outgoing)
        .await
        .map_err(|_| PluginError::transport("The image endpoint could not be reached."))?;
    transport::finish_response(response, secrets)
        .await
        .map(Some)
}

pub fn prepare(
    oauth: bool,
    target: &mut Url,
    headers: &mut BTreeMap<String, String>,
    body: &mut Option<Vec<u8>>,
) -> PluginResult<()> {
    let path = target.path().to_string();
    let image = path.ends_with("/images/generations") || path.ends_with("/images/edits");
    if !image
        && !path.ends_with("/embeddings")
        && !path.contains("/audio/")
        && !path.contains("/videos")
    {
        return Ok(());
    }
    if oauth && !image {
        return Err(PluginError::invalid(
            "This operation requires an OpenAI API account.",
        ));
    }
    if oauth {
        target.set_path(&format!(
            "/backend-api/codex/images/{}",
            path.rsplit('/').next().unwrap_or("generations")
        ));
        headers.insert("accept".into(), "application/json".into());
        headers.insert("originator".into(), "codex_cli_rs".into());
        for name in ["x-codex-imagegen-request-id", "x-codex-image-turn-id"] {
            headers
                .entry(name.into())
                .or_insert_with(|| uuid::Uuid::new_v4().to_string());
        }
    } else if target.host_str() == Some("api.openai.com") && !path.starts_with("/v1/") {
        target.set_path(&format!("/v1{path}"));
    }
    if !image
        || body.is_none()
        || headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v.starts_with("multipart/"))
    {
        return Ok(());
    }
    let mut value: Value = serde_json::from_slice(body.as_ref().unwrap())
        .map_err(|_| PluginError::invalid("The image request is not valid JSON."))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| PluginError::invalid("The image request must be an object."))?;
    let model = object
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if model.starts_with("gpt-image") || model.starts_with("chatgpt-image") {
        object.remove("response_format");
    }
    if let Some(aspect) = object.remove("aspect_ratio") {
        let size = match aspect.as_str() {
            Some("1:1") => "1024x1024",
            Some("16:9" | "3:2" | "4:3") => "1536x1024",
            Some("9:16" | "2:3" | "3:4") => "1024x1536",
            Some("auto") => "auto",
            _ => {
                return Err(PluginError::invalid(
                    "The OpenAI image aspect ratio is unsupported.",
                ))
            }
        };
        object.entry("size").or_insert(json!(size));
    }
    let references = nora_provider::media_upload::images_from_fields(object)?;
    object.remove("image");
    object.remove("images");
    if !references.is_empty() {
        let images = references
            .into_iter()
            .map(|reference| match reference {
                nora_provider::media_upload::ImageReference::Url(url) => json!({"image_url":url}),
                nora_provider::media_upload::ImageReference::FileId(id) => json!({"file_id":id}),
            })
            .collect::<Vec<_>>();
        object.insert("images".into(), json!(images));
        if target.path().ends_with("/generations") {
            target.set_path(&format!(
                "{}/edits",
                target.path().trim_end_matches("/generations")
            ));
        }
    }
    if let Some(mask) = object.get("mask") {
        if !mask.is_null() {
            let mut masks = nora_provider::media_upload::image_sources(mask)?;
            if masks.len() != 1 {
                return Err(PluginError::invalid("Supply exactly one mask image."));
            }
            let mask = match masks.remove(0) {
                nora_provider::media_upload::ImageReference::Url(url) => json!({"image_url":url}),
                nora_provider::media_upload::ImageReference::FileId(id) => json!({"file_id":id}),
            };
            object.insert("mask".into(), mask);
        }
    }
    *body = Some(
        serde_json::to_vec(&value)
            .map_err(|_| PluginError::invalid("The image request could not be encoded."))?,
    );
    Ok(())
}

#[cfg(test)]
#[path = "../test/media.test.rs"]
mod tests;
