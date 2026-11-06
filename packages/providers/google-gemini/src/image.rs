use crate::provider::{json_response, native_request, send_json, JsonResponse};
use base64::{engine::general_purpose::STANDARD, Engine};
use futures::StreamExt;
use nora_ai::{HttpClient, HttpMethod, HttpRequest};
use nora_provider::media_upload::{
    images_from_fields, ImageReference, MediaBody, MediaFile, MAX_MEDIA_BODY_BYTES,
};
use nora_provider_plugin_sdk::{FetchResponse, PluginError, PluginResult};
use serde_json::{json, Map, Value};
use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};
use url::Url;

const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_INPUT_IMAGES: usize = 32;

pub async fn fetch(
    template: HttpRequest,
    body: MediaBody,
    edit: bool,
    client: &dyn HttpClient,
) -> PluginResult<FetchResponse> {
    let (fields, uploads) = fields(body)?;
    let model = fields
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| PluginError::invalid("Name the Google image model."))?;
    if model.trim_start_matches("models/").starts_with("imagen-") {
        let mut response = json_response(
            BTreeMap::new(),
            json!({"error":{"type":"model_retired","message":"Google has shut down Imagen in the Gemini API. Choose a currently available Gemini image model."}}),
        )?;
        response.status = 410;
        return Ok(response);
    }
    let prompt = fields
        .get("prompt")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| PluginError::invalid("Describe the image in prompt."))?;
    let count = integer(fields.get("n"), 1)?;
    if !(1..=4).contains(&count) {
        return Err(PluginError::invalid("Request between 1 and 4 images."));
    }
    let response_format = fields
        .get("response_format")
        .and_then(Value::as_str)
        .unwrap_or("b64_json");
    if !matches!(response_format, "b64_json" | "url") {
        return Err(PluginError::invalid(
            "Image response_format must be b64_json or url.",
        ));
    }
    if fields.get("mask").is_some_and(|value| !value.is_null())
        || uploads.iter().any(|file| file.field == "mask")
    {
        return Err(PluginError::invalid(
            "Gemini image edits do not support an image mask.",
        ));
    }
    let mut parts = vec![json!({"text":prompt})];
    let mut input_budget = ReferenceBudget::new(MAX_MEDIA_BODY_BYTES);
    if uploads.len() > MAX_INPUT_IMAGES {
        return Err(PluginError::invalid(
            "At most 32 reference images can be supplied.",
        ));
    }
    for file in uploads {
        if !matches!(file.field.as_str(), "image" | "image[]" | "images") {
            return Err(PluginError::invalid(
                "Gemini image edits accept only image uploads.",
            ));
        }
        parts.push(inline_image(
            &file.mime_type,
            &file.bytes,
            &mut input_budget,
        )?);
    }
    let references = images_from_fields(&fields)?;
    if parts.len().saturating_sub(1) + references.len() > MAX_INPUT_IMAGES {
        return Err(PluginError::invalid(
            "At most 32 reference images can be supplied.",
        ));
    }
    for reference in references {
        let ImageReference::Url(reference) = reference else {
            return Err(PluginError::invalid("Gemini reference images require data or HTTP URLs; an OpenAI file_id cannot be used with Google."));
        };
        let (mime, bytes) = reference_image(&reference, client, input_budget.remaining()).await?;
        parts.push(inline_image(&mime, &bytes, &mut input_budget)?);
    }
    if edit && parts.len() == 1 {
        return Err(PluginError::invalid(
            "An image edit requires a reference image.",
        ));
    }
    let mut configuration = json!({"responseModalities":["TEXT","IMAGE"]});
    let mut image_config = Map::new();
    if let Some(ratio) = aspect_ratio(&fields)? {
        image_config.insert("aspectRatio".into(), json!(ratio));
    }
    if let Some(size) = fields
        .get("image_size")
        .or_else(|| fields.get("resolution"))
        .and_then(Value::as_str)
    {
        if !matches!(size, "512" | "1K" | "2K" | "4K") {
            return Err(PluginError::invalid(
                "The Google image resolution is invalid.",
            ));
        }
        image_config.insert("imageSize".into(), json!(size));
    }
    if !image_config.is_empty() {
        configuration["imageConfig"] = Value::Object(image_config);
    }
    let native =
        json!({"contents":[{"role":"user","parts":parts}],"generationConfig":configuration});
    let outgoing = native_request(template, model, "generateContent", native)?;
    let mut output = Vec::new();
    let mut prompt_tokens = 0u64;
    let mut output_tokens = 0u64;
    let mut total_tokens = 0u64;
    let mut output_bytes = 0usize;
    let mut headers = BTreeMap::new();
    let mut first_failure: Option<PluginResult<FetchResponse>> = None;
    for _ in 0..count {
        let response = match send_json(client, outgoing.clone()).await {
            Ok(response) => response,
            Err(error) => {
                first_failure.get_or_insert(Err(error));
                continue;
            }
        };
        let (value, next_headers) = match response {
            JsonResponse::Error(error) => {
                first_failure.get_or_insert(Ok(error));
                continue;
            }
            JsonResponse::Value(value, headers) => (value, headers),
        };
        headers = next_headers;
        let usage = value
            .get("usageMetadata")
            .or_else(|| value.get("usage_metadata"));
        let input = usage
            .and_then(|value| value.get("promptTokenCount"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let out = usage
            .and_then(|value| value.get("candidatesTokenCount"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        prompt_tokens = prompt_tokens.saturating_add(input);
        output_tokens = output_tokens.saturating_add(out);
        total_tokens = total_tokens.saturating_add(
            usage
                .and_then(|value| value.get("totalTokenCount"))
                .and_then(Value::as_u64)
                .unwrap_or(input.saturating_add(out)),
        );
        let mut text = String::new();
        let mut images = Vec::new();
        if let Some(candidates) = value.get("candidates").and_then(Value::as_array) {
            for candidate in candidates {
                for part in candidate
                    .pointer("/content/parts")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if part.get("thought").and_then(Value::as_bool) == Some(true) {
                        continue;
                    }
                    if let Some(piece) = part.get("text").and_then(Value::as_str) {
                        text.push_str(piece);
                    }
                    if let Some(image) = part.get("inlineData").or_else(|| part.get("inline_data"))
                    {
                        let mime = image
                            .get("mimeType")
                            .or_else(|| image.get("mime_type"))
                            .and_then(Value::as_str)
                            .unwrap_or("image/png");
                        let Some(data) = image.get("data").and_then(Value::as_str) else {
                            first_failure.get_or_insert(Err(invalid_image()));
                            continue;
                        };
                        if let Err(error) = validate_image_data(mime, data) {
                            first_failure.get_or_insert(Err(error));
                            continue;
                        }
                        let next_bytes = output_bytes.saturating_add(data.len());
                        if next_bytes > crate::provider::MAX_RESPONSE_BYTES {
                            first_failure.get_or_insert(Err(PluginError::transport(
                                "The combined Google image response exceeds 256 MiB.",
                            )));
                            continue;
                        }
                        output_bytes = next_bytes;
                        images.push(json!({"b64_json":data,"mime_type":mime}));
                    }
                }
            }
        }
        if images.is_empty() {
            first_failure.get_or_insert(Err(PluginError::transport("Google returned no image. The prompt may have been blocked or the selected model may not generate images.")));
            continue;
        }
        for mut image in images {
            if !text.trim().is_empty() {
                image["revised_prompt"] = json!(text.trim());
            }
            output.push(image);
        }
    }
    if output.is_empty() {
        return first_failure.unwrap_or_else(|| Err(invalid_image()));
    }
    let mut result = json!({"created":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),"data":output});
    if total_tokens > 0 {
        result["usage"] = json!({"prompt_tokens":prompt_tokens,"completion_tokens":output_tokens,"total_tokens":total_tokens});
    }
    json_response(headers, result)
}

fn fields(body: MediaBody) -> PluginResult<(Map<String, Value>, Vec<MediaFile>)> {
    match body {
        MediaBody::Json(Value::Object(value)) => Ok((value, Vec::new())),
        MediaBody::Multipart(form) => Ok((
            form.fields
                .into_iter()
                .map(|(key, value)| (key, Value::String(value)))
                .collect(),
            form.files,
        )),
        _ => Err(PluginError::invalid(
            "The image request must be a JSON object or multipart form.",
        )),
    }
}

fn integer(value: Option<&Value>, default: u64) -> PluginResult<u64> {
    match value {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .or_else(|| value.as_str()?.parse().ok())
            .ok_or_else(|| {
                PluginError::invalid("The number of images must be a positive integer.")
            }),
    }
}

fn aspect_ratio(fields: &Map<String, Value>) -> PluginResult<Option<String>> {
    if let Some(value) = fields.get("aspect_ratio") {
        let ratio = value
            .as_str()
            .filter(|value| {
                value.split_once(':').is_some_and(|(w, h)| {
                    w.parse::<u32>().is_ok_and(|n| n > 0) && h.parse::<u32>().is_ok_and(|n| n > 0)
                })
            })
            .ok_or_else(|| PluginError::invalid("The image aspect ratio must be width:height."))?;
        return Ok(Some(ratio.into()));
    }
    let Some(size) = fields.get("size").and_then(Value::as_str) else {
        return Ok(None);
    };
    let size = size.trim().to_ascii_lowercase();
    if size.is_empty() || size == "auto" {
        return Ok(None);
    }
    if size.contains(':') {
        return aspect_ratio(&Map::from_iter([("aspect_ratio".into(), json!(size))]));
    }
    let (width, height) = size
        .split_once('x')
        .ok_or_else(|| PluginError::invalid("The image size must be widthxheight."))?;
    let dimension = |value: &str| {
        value
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && *value > 0.0)
            .ok_or_else(invalid_image)
    };
    let ratio = dimension(width)?.ln() - dimension(height)?.ln();
    let ratios = [
        ("1:1", 1.0_f64),
        ("2:3", 2.0 / 3.0),
        ("3:2", 3.0 / 2.0),
        ("3:4", 3.0 / 4.0),
        ("4:3", 4.0 / 3.0),
        ("4:5", 4.0 / 5.0),
        ("5:4", 5.0 / 4.0),
        ("9:16", 9.0 / 16.0),
        ("16:9", 16.0 / 9.0),
        ("21:9", 21.0 / 9.0),
    ];
    Ok(ratios
        .into_iter()
        .min_by(|(_, left), (_, right)| {
            (ratio - left.ln())
                .abs()
                .total_cmp(&(ratio - right.ln()).abs())
        })
        .map(|(name, _)| name.into()))
}

struct ReferenceBudget {
    used: usize,
    limit: usize,
}

impl ReferenceBudget {
    fn new(limit: usize) -> Self {
        Self { used: 0, limit }
    }

    fn remaining(&self) -> usize {
        self.limit.saturating_sub(self.used)
    }

    fn reserve(&mut self, mime: &str, bytes: usize) -> PluginResult<()> {
        let overhead = json!({"inlineData":{"mimeType":mime,"data":""}})
            .to_string()
            .len();
        let size = encoded_length(bytes).saturating_add(overhead);
        if size > self.remaining() {
            return Err(input_too_large());
        }
        self.used += size;
        Ok(())
    }
}

fn encoded_length(bytes: usize) -> usize {
    bytes.div_ceil(3).saturating_mul(4)
}

fn input_too_large() -> PluginError {
    PluginError::invalid("The combined inline reference images exceed 512 MiB.")
}

fn inline_image(mime: &str, bytes: &[u8], budget: &mut ReferenceBudget) -> PluginResult<Value> {
    if bytes.is_empty()
        || bytes.len() > MAX_IMAGE_BYTES
        || !mime.starts_with("image/")
        || mime.len() > 256
    {
        return Err(invalid_image());
    }
    budget.reserve(mime, bytes.len())?;
    Ok(json!({"inlineData":{"mimeType":mime,"data":STANDARD.encode(bytes)}}))
}

fn validate_image_data(mime: &str, data: &str) -> PluginResult<()> {
    if data.len() > encoded_length(MAX_IMAGE_BYTES)
        || !mime.starts_with("image/")
        || mime.len() > 256
    {
        return Err(invalid_image());
    }
    let bytes = STANDARD.decode(data).map_err(|_| invalid_image())?;
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        return Err(invalid_image());
    }
    Ok(())
}

async fn reference_image(
    reference: &str,
    client: &dyn HttpClient,
    remaining_encoded: usize,
) -> PluginResult<(String, Vec<u8>)> {
    if let Some(data) = reference.strip_prefix("data:") {
        let (mime, data) = data.split_once(";base64,").ok_or_else(invalid_image)?;
        if data.len() > remaining_encoded {
            return Err(input_too_large());
        }
        if data.len() > encoded_length(MAX_IMAGE_BYTES)
            || !mime.starts_with("image/")
            || mime.len() > 256
        {
            return Err(invalid_image());
        }
        let bytes = STANDARD.decode(data).map_err(|_| invalid_image())?;
        if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
            return Err(invalid_image());
        }
        return Ok((mime.into(), bytes));
    }
    let url = Url::parse(reference).map_err(|_| invalid_image())?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid_image());
    }
    let mut response = client
        .stream(HttpRequest {
            method: HttpMethod::Get,
            url: url.into(),
            headers: BTreeMap::new(),
            body: None,
        })
        .await
        .map_err(|_| PluginError::transport("The reference image could not be downloaded."))?;
    if response.status >= 300 {
        return Err(PluginError::transport(
            "The reference image could not be downloaded.",
        ));
    }
    let mime = response
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| value.split(';').next().unwrap_or("").trim().to_owned())
        .unwrap_or_default();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.body.next().await {
        let chunk =
            chunk.map_err(|_| PluginError::transport("The reference image download stopped."))?;
        let next_bytes = bytes.len().saturating_add(chunk.len());
        if next_bytes > MAX_IMAGE_BYTES {
            return Err(invalid_image());
        }
        if encoded_length(next_bytes) > remaining_encoded {
            return Err(input_too_large());
        }
        bytes.extend_from_slice(&chunk);
    }
    let sniffed = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    };
    let mime = sniffed.unwrap_or(&mime).to_owned();
    if bytes.is_empty() || !mime.starts_with("image/") {
        return Err(invalid_image());
    }
    Ok((mime, bytes))
}

fn invalid_image() -> PluginError {
    PluginError::invalid("A reference or generated image is invalid or exceeds 64 MiB.")
}

#[cfg(test)]
#[path = "../test/image.test.rs"]
mod tests;
