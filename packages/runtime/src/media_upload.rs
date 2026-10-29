use base64::{engine::general_purpose::STANDARD, Engine};
use bytes::{Bytes, BytesMut};
use futures::{stream, Stream, StreamExt};
use nora_provider_plugin_sdk::{FetchRequest, PluginError, PluginResult};
use serde_json::{Map, Value};
use std::{collections::BTreeMap, pin::Pin};
use tokio::io::AsyncReadExt;

pub const MAX_MEDIA_BODY_BYTES: usize = 512 * 1024 * 1024;
const MAX_FORM_FIELDS: usize = 200;
const MAX_TEXT_FIELD_BYTES: usize = 1024 * 1024;
const MAX_IMAGE_SOURCES: usize = 32;
const MAX_SOURCE_DEPTH: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageReference {
    Url(String),
    FileId(String),
}

/// Accept the image aliases used by OpenAI-compatible clients without choosing
/// a supplier's wire shape. An object must identify exactly one source.
pub fn image_sources(value: &Value) -> PluginResult<Vec<ImageReference>> {
    let mut output = Vec::new();
    let mut bytes = 0;
    append_sources(value, &mut output, &mut bytes, 0)?;
    Ok(output)
}

/// As in Magpie, references in `image` precede references in `images` when both
/// aliases are present. Neither alias silently overwrites the other.
pub fn images_from_fields(fields: &Map<String, Value>) -> PluginResult<Vec<ImageReference>> {
    let mut output = Vec::new();
    let mut bytes = 0;
    for key in ["image", "images"] {
        if let Some(value) = fields.get(key) {
            append_sources(value, &mut output, &mut bytes, 0)?;
        }
    }
    Ok(output)
}

fn append_sources(
    value: &Value,
    output: &mut Vec<ImageReference>,
    bytes: &mut usize,
    depth: usize,
) -> PluginResult<()> {
    if depth > MAX_SOURCE_DEPTH {
        return Err(invalid_sources());
    }
    match value {
        Value::Null => Ok(()),
        Value::String(value) => append_source(value, false, output, bytes),
        Value::Array(values) => {
            for value in values {
                append_sources(value, output, bytes, depth + 1)?;
            }
            Ok(())
        }
        Value::Object(fields) => {
            let mut sources = ["url", "image_url", "file_id"]
                .into_iter()
                .filter_map(|key| {
                    fields
                        .get(key)
                        .filter(|value| !value.is_null())
                        .map(|value| (key, value))
                });
            let (key, value) = sources.next().ok_or_else(invalid_sources)?;
            if sources.next().is_some() {
                return Err(invalid_sources());
            }
            if key == "file_id" {
                let value = value.as_str().ok_or_else(invalid_sources)?;
                return append_source(value, true, output, bytes);
            }
            append_sources(value, output, bytes, depth + 1)
        }
        _ => Err(invalid_sources()),
    }
}

fn append_source(
    value: &str,
    file_id: bool,
    output: &mut Vec<ImageReference>,
    bytes: &mut usize,
) -> PluginResult<()> {
    let value = value.trim();
    if value.is_empty()
        || value.chars().any(char::is_control)
        || output.len() >= MAX_IMAGE_SOURCES
        || (file_id && value.len() > 512)
    {
        return Err(invalid_sources());
    }
    *bytes = (*bytes).saturating_add(value.len());
    if *bytes > MAX_MEDIA_BODY_BYTES {
        return Err(invalid_sources());
    }
    output.push(if file_id {
        ImageReference::FileId(value.into())
    } else {
        ImageReference::Url(value.into())
    });
    Ok(())
}

fn invalid_sources() -> PluginError {
    PluginError::invalid("Supply at most 32 image references. Each reference must contain exactly one URL or file_id.")
}

/// Parsed without changing field names or deciding how a supplier represents images.
pub enum MediaBody {
    Json(Value),
    Multipart(MediaForm),
}

pub struct MediaForm {
    pub fields: BTreeMap<String, String>,
    pub files: Vec<MediaFile>,
}

pub struct MediaFile {
    pub field: String,
    pub filename: Option<String>,
    pub mime_type: String,
    pub bytes: Bytes,
}

impl MediaFile {
    pub fn data_url(&self) -> String {
        format!(
            "data:{};base64,{}",
            self.mime_type,
            STANDARD.encode(&self.bytes)
        )
    }
}

type BodyStream = Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>;

/// Body files are verified before parsing and streamed from that same file handle.
/// A vendor that requires inline JSON may then encode only its accepted uploads.
pub async fn parse(request: &FetchRequest) -> PluginResult<MediaBody> {
    let content_type = request
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| value.as_str())
        .unwrap_or("application/json");
    let input = body_stream(request).await?;
    if content_type
        .to_ascii_lowercase()
        .starts_with("multipart/form-data")
    {
        let boundary = multer::parse_boundary(content_type)
            .map_err(|_| PluginError::invalid("The image upload boundary is invalid."))?;
        return parse_form(input, boundary).await.map(MediaBody::Multipart);
    }
    if !content_type
        .split(';')
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
    {
        return Err(PluginError::invalid(
            "This media operation requires JSON or multipart/form-data.",
        ));
    }
    let mut input = input;
    let mut bytes = Vec::new();
    while let Some(chunk) = input.next().await {
        let chunk = chunk.map_err(|_| invalid_body())?;
        if bytes.len().saturating_add(chunk.len()) > MAX_MEDIA_BODY_BYTES {
            return Err(invalid_body());
        }
        bytes.extend_from_slice(&chunk);
    }
    let value = serde_json::from_slice::<Value>(&bytes)
        .map_err(|_| PluginError::invalid("The media request body must be a JSON object."))?;
    if !value.is_object() {
        return Err(PluginError::invalid(
            "The media request body must be a JSON object.",
        ));
    }
    Ok(MediaBody::Json(value))
}

async fn body_stream(request: &FetchRequest) -> PluginResult<BodyStream> {
    if let Some(descriptor) = &request.body_file {
        if request.body.is_some() {
            return Err(PluginError::invalid(
                "A media request cannot contain both a body and a body file.",
            ));
        }
        let length = descriptor.length;
        let descriptor = descriptor.clone();
        let file = tokio::task::spawn_blocking(move || descriptor.open_verified())
            .await
            .map_err(|_| invalid_body())??;
        return Ok(Box::pin(tokio_util::io::ReaderStream::new(
            tokio::fs::File::from_std(file).take(length),
        )));
    }
    let bytes = request.decode_body()?.unwrap_or_default();
    if bytes.len() > MAX_MEDIA_BODY_BYTES {
        return Err(invalid_body());
    }
    Ok(Box::pin(stream::once(
        async move { Ok(Bytes::from(bytes)) },
    )))
}

async fn parse_form(input: BodyStream, boundary: String) -> PluginResult<MediaForm> {
    let input = prime_form_stream(input, &boundary).await?;
    let constraints = multer::Constraints::new().size_limit(
        multer::SizeLimit::new()
            .whole_stream(MAX_MEDIA_BODY_BYTES as u64)
            .per_field(MAX_MEDIA_BODY_BYTES as u64),
    );
    let mut multipart = multer::Multipart::with_constraints(input, boundary, constraints);
    let mut result = MediaForm {
        fields: BTreeMap::new(),
        files: Vec::new(),
    };
    let mut count = 0;
    while let Some(mut field) = multipart.next_field().await.map_err(|_| invalid_form())? {
        count += 1;
        if count > MAX_FORM_FIELDS {
            return Err(invalid_form());
        }
        let name = field
            .name()
            .filter(|name| {
                !name.is_empty() && name.len() <= 512 && !name.chars().any(char::is_control)
            })
            .ok_or_else(invalid_form)?
            .to_owned();
        let filename = field.file_name().map(str::to_owned);
        let mime_type = field
            .content_type()
            .map(ToString::to_string)
            .unwrap_or_else(|| "application/octet-stream".into());
        let is_file = filename.is_some();
        if is_file && name == "model" {
            return Err(invalid_form());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = field.chunk().await.map_err(|_| invalid_form())? {
            if bytes.len().saturating_add(chunk.len())
                > if is_file {
                    MAX_MEDIA_BODY_BYTES
                } else {
                    MAX_TEXT_FIELD_BYTES
                }
            {
                return Err(invalid_form());
            }
            bytes.extend_from_slice(&chunk);
        }
        if is_file {
            result.files.push(MediaFile {
                field: name,
                filename,
                mime_type,
                bytes: Bytes::from(bytes),
            });
        } else {
            let value = String::from_utf8(bytes).map_err(|_| invalid_form())?;
            if result.fields.insert(name, value).is_some() {
                return Err(invalid_form());
            }
        }
    }
    Ok(result)
}

async fn prime_form_stream(mut input: BodyStream, boundary: &str) -> PluginResult<BodyStream> {
    let mut first = loop {
        let chunk = input
            .next()
            .await
            .ok_or_else(invalid_form)?
            .map_err(|_| invalid_form())?;
        if !chunk.is_empty() {
            break chunk;
        }
    };
    if first.len() > MAX_MEDIA_BODY_BYTES {
        return Err(invalid_form());
    }
    // Multer 3.1 can report EOF without rechecking its buffer if the initial
    // boundary arrives between its two polls of a previously pending stream.
    let marker = format!("--{boundary}");
    let contains_boundary = |bytes: &[u8]| {
        bytes
            .windows(marker.len())
            .any(|window| window == marker.as_bytes())
    };
    if !contains_boundary(&first) {
        let mut prefix = BytesMut::from(first.as_ref());
        while !contains_boundary(&prefix) {
            let chunk = input
                .next()
                .await
                .ok_or_else(invalid_form)?
                .map_err(|_| invalid_form())?;
            if prefix.len().saturating_add(chunk.len()) > MAX_MEDIA_BODY_BYTES {
                return Err(invalid_form());
            }
            prefix.extend_from_slice(&chunk);
        }
        first = prefix.freeze();
    }
    Ok(Box::pin(
        stream::once(async move { Ok(first) }).chain(input),
    ))
}

fn invalid_body() -> PluginError {
    PluginError::invalid("The media request body is missing, changed, or exceeds 512 MiB.")
}
fn invalid_form() -> PluginError {
    PluginError::invalid(
        "The media upload has invalid or duplicate fields, or exceeds its size limit.",
    )
}

#[cfg(test)]
#[path = "../test/media_upload.test.rs"]
mod tests;
