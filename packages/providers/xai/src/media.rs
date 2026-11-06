use base64::Engine;
use nora_ai::{HttpClient, HttpMethod, HttpRequest};
use nora_provider::{definition::Definition, transport};
use nora_provider_plugin_sdk::{FetchRequest, FetchResponse, PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use url::Url;

pub fn prepare(
    target: &mut Url,
    headers: &mut BTreeMap<String, String>,
    body: &mut Option<Vec<u8>>,
) -> PluginResult<bool> {
    let path = target.path().to_string();
    let video = path.trim_end_matches('/').ends_with("/videos");
    let image = path.ends_with("/images/generations") || path.ends_with("/images/edits");
    if !video && !image {
        return Ok(path.contains("/videos/"));
    }
    if video {
        target.set_path(&format!("{}/generations", path.trim_end_matches('/')));
    }
    let Some(bytes) = body.as_ref() else {
        return Ok(true);
    };
    let mut value: Value = serde_json::from_slice(bytes)
        .map_err(|_| PluginError::invalid("The Grok media request is not valid JSON."))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| PluginError::invalid("The Grok media request must be an object."))?;
    if video {
        if let Some(seconds) = object.remove("seconds") {
            if !object.contains_key("duration") {
                object.insert("duration".into(), seconds);
            }
        }
        if let Some(duration) = object.get("duration") {
            let seconds = duration
                .as_f64()
                .or_else(|| {
                    duration
                        .as_str()
                        .and_then(|text| serde_json::from_str::<serde_json::Number>(text).ok())
                        .and_then(|n| n.as_f64())
                })
                .filter(|n| n.is_finite() && *n >= 1.0 && *n <= i32::MAX as f64 && n.fract() == 0.0)
                .ok_or_else(|| {
                    PluginError::invalid("Video duration must be a whole number of seconds.")
                })?;
            object.insert("duration".into(), json!(seconds as u32));
        }
        object.remove("response_format");
    } else {
        object.insert("response_format".into(), json!("b64_json"));
        if object.get("mask").is_some_and(|mask| !mask.is_null()) {
            return Err(PluginError::invalid(
                "Grok image edits do not support masks.",
            ));
        }
        let references = nora_provider::media_upload::images_from_fields(object)?;
        object.remove("image");
        object.remove("images");
        if !references.is_empty() {
            if references.len() > 5 {
                return Err(PluginError::invalid(
                    "Grok image edits support at most five reference images.",
                ));
            }
            let images = references
                .into_iter()
                .map(|reference| match reference {
                    nora_provider::media_upload::ImageReference::Url(url) => {
                        json!({"url":url,"type":"image_url"})
                    }
                    nora_provider::media_upload::ImageReference::FileId(id) => {
                        json!({"file_id":id})
                    }
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
    }
    if let Some(size) = object
        .remove("size")
        .and_then(|v| v.as_str().map(str::to_string))
    {
        if let Some(aspect) = aspect_ratio(&size, video) {
            object.entry("aspect_ratio").or_insert(json!(aspect));
            if video {
                if let Some((width, height)) = dimensions(&size) {
                    let short = width.min(height);
                    let mut resolution = if short >= 1080.0 {
                        "1080p"
                    } else if short >= 720.0 {
                        "720p"
                    } else {
                        "480p"
                    };
                    if resolution == "1080p"
                        && object.get("model").and_then(Value::as_str) == Some("grok-imagine-video")
                    {
                        resolution = "720p";
                    }
                    object.entry("resolution").or_insert(json!(resolution));
                }
            }
        } else {
            return Err(PluginError::invalid(
                "Grok media requires an aspect ratio rather than pixel dimensions.",
            ));
        }
    }
    headers.insert("accept".into(), "application/json".into());
    *body = Some(
        serde_json::to_vec(&value)
            .map_err(|_| PluginError::invalid("The Grok media request could not be encoded."))?,
    );
    Ok(true)
}

fn dimensions(size: &str) -> Option<(f64, f64)> {
    let size = size.trim().to_ascii_lowercase();
    let (w, h) = size.split_once('x')?;
    let (w, h) = (w.parse::<f64>().ok()?, h.parse::<f64>().ok()?);
    (w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0).then_some((w, h))
}
fn aspect_ratio(size: &str, video: bool) -> Option<String> {
    if size.contains(':') {
        return Some(size.trim().into());
    }
    let (w, h) = dimensions(size)?;
    let ratios = if video {
        &["1:1", "16:9", "9:16", "4:3", "3:4", "3:2", "2:3"][..]
    } else {
        &[
            "1:1", "16:9", "9:16", "4:3", "3:4", "3:2", "2:3", "2:1", "1:2", "19.5:9", "9:19.5",
            "20:9", "9:20",
        ][..]
    };
    ratios
        .iter()
        .min_by(|left, right| {
            let distance = |ratio: &&str| {
                let (a, b) = ratio.split_once(':').unwrap();
                ((w / h).ln() - (a.parse::<f64>().unwrap() / b.parse::<f64>().unwrap()).ln()).abs()
            };
            distance(left).total_cmp(&distance(right))
        })
        .map(|value| (*value).into())
}

pub async fn fetch(
    definition: &Definition,
    request: &FetchRequest,
    client: &dyn HttpClient,
) -> PluginResult<Option<FetchResponse>> {
    let url = Url::parse(&request.url)
        .map_err(|_| PluginError::invalid("The Grok request address is invalid."))?;
    let path = url.path().trim_end_matches('/');
    if path.ends_with("/images/edits")
        && request.headers.iter().any(|(key, value)| {
            key.eq_ignore_ascii_case("content-type")
                && value
                    .to_ascii_lowercase()
                    .starts_with("multipart/form-data")
        })
    {
        let nora_provider::media_upload::MediaBody::Multipart(form) =
            nora_provider::media_upload::parse(request).await?
        else {
            return Ok(None);
        };
        let mut value = serde_json::Map::new();
        for (key, text) in form.fields {
            value.insert(
                key.clone(),
                if key == "n" {
                    json!(text.parse::<u32>().ok().filter(|n| *n > 0).ok_or_else(|| {
                        PluginError::invalid("The image count must be a positive whole number.")
                    })?)
                } else {
                    json!(text)
                },
            );
        }
        let mut images = Vec::new();
        for file in form.files {
            if !file.mime_type.starts_with("image/")
                || !matches!(
                    file.field.as_str(),
                    "image" | "image[]" | "images" | "images[]"
                )
            {
                return Err(PluginError::invalid(
                    "Grok image edits only accept image files and do not support masks.",
                ));
            }
            images.push(json!({"url":file.data_url(),"type":"image_url"}));
        }
        if images.is_empty() {
            return Err(PluginError::invalid(
                "An image edit needs at least one image.",
            ));
        }
        value.insert("images".into(), json!(images));
        let mut outgoing = request.clone();
        outgoing.body_file = None;
        outgoing.body = Some(base64::engine::general_purpose::STANDARD.encode(
            serde_json::to_vec(&value).map_err(|_| {
                PluginError::invalid("The image edit request could not be encoded.")
            })?,
        ));
        outgoing.headers.retain(|key, _| {
            !key.eq_ignore_ascii_case("content-type") && !key.eq_ignore_ascii_case("content-length")
        });
        outgoing
            .headers
            .insert("content-type".into(), "application/json".into());
        let outgoing = transport::build_request(definition, outgoing).await?;
        let secrets = transport::request_secrets(&outgoing);
        let response = client
            .stream(outgoing)
            .await
            .map_err(|_| PluginError::transport("The image endpoint could not be reached."))?;
        return transport::finish_response(response, secrets)
            .await
            .map(Some);
    }
    if !path.contains("/videos") {
        return Ok(None);
    }
    let content = path.ends_with("/content");
    let mut outgoing = request.clone();
    if content {
        let mut url = url.clone();
        url.set_path(path.trim_end_matches("/content"));
        outgoing.url = url.into();
    }
    let outgoing = transport::build_request(definition, outgoing).await?;
    let secrets = transport::request_secrets(&outgoing);
    let response = client
        .stream(outgoing)
        .await
        .map_err(|_| PluginError::transport("The Grok video endpoint could not be reached."))?;
    if response.status >= 400 {
        return transport::finish_response(response, secrets)
            .await
            .map(Some);
    }
    let status = response.status;
    let mut headers = response.headers;
    let bytes = nora_ai::collect_http_body_text_limited(response.body, 1024 * 1024 + 1)
        .await
        .map_err(|_| PluginError::transport("The Grok video response stopped."))?;
    if bytes.len() > 1024 * 1024 {
        return Err(PluginError::transport(
            "The Grok video response is too large.",
        ));
    }
    let mut value: Value = serde_json::from_str(&bytes)
        .map_err(|_| PluginError::transport("The Grok video response is invalid."))?;
    if content {
        if !matches!(
            value.get("status").and_then(Value::as_str),
            Some("done" | "succeeded" | "completed")
        ) {
            return Ok(Some(FetchResponse::bytes(
                409,
                BTreeMap::from([("content-type".into(), "application/json".into())]),
                br#"{"error":{"message":"The video is not ready."}}"#.to_vec(),
            )));
        }
        let address = value
            .pointer("/video/url")
            .or_else(|| value.pointer("/content/video_url"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                PluginError::transport("The completed video has no download address.")
            })?;
        let url = Url::parse(address)
            .map_err(|_| PluginError::transport("The video download address is invalid."))?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || !public_host(&url)
        {
            return Err(PluginError::transport(
                "The video download address is not public HTTPS.",
            ));
        }
        // Download addresses are supplied by the vendor; account authorization stays on its API.
        let response = client
            .stream(HttpRequest::new(HttpMethod::Get, url.to_string()))
            .await
            .map_err(|_| PluginError::transport("The generated video could not be downloaded."))?;
        return transport::finish_response(response, Vec::new())
            .await
            .map(Some);
    }
    normalize_video(&mut value)?;
    headers.retain(|key, _| {
        !matches!(
            key.to_ascii_lowercase().as_str(),
            "content-length" | "content-encoding"
        )
    });
    headers.insert("content-type".into(), "application/json".into());
    Ok(Some(FetchResponse::bytes(
        status,
        headers,
        serde_json::to_vec(&value)
            .map_err(|_| PluginError::transport("The video response could not be encoded."))?,
    )))
}

fn normalize_video(value: &mut Value) -> PluginResult<()> {
    if !value.is_object() {
        return Err(PluginError::transport(
            "The Grok video response must be an object.",
        ));
    }
    if value.get("id").is_none() {
        if let Some(id) = value.get("request_id").cloned() {
            value["id"] = id;
        }
    }
    let state = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let progress = value
        .get("progress")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .unwrap_or(0.0)
        .clamp(0.0, 100.0) as u64;
    let status = match state.as_str() {
        "done" | "succeeded" | "completed" => "completed",
        "running" | "processing" | "in_progress" => "in_progress",
        "failed" | "expired" | "cancelled" => "failed",
        _ if progress > 0 => "in_progress",
        _ => "queued",
    };
    value["object"] = json!("video");
    value["status"] = json!(status);
    value["progress"] = json!(if status == "completed" { 100 } else { progress });
    if value.get("created_at").and_then(Value::as_u64).is_none() {
        value["created_at"] = json!(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs());
    }
    if status == "failed" {
        let (code, message) = match state.as_str() {
            "expired" => ("expired", "The vendor no longer keeps this video."),
            "cancelled" => ("cancelled", "The video was cancelled."),
            _ => (
                value
                    .pointer("/error/code")
                    .and_then(Value::as_str)
                    .unwrap_or("video_failed"),
                value
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                    .unwrap_or("The video failed."),
            ),
        };
        value["error"] = json!({"code":code,"message":message});
    }
    if let Some(duration) = value
        .pointer("/video/duration")
        .or_else(|| value.get("duration"))
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n > 0.0)
    {
        value["seconds"] = json!(duration.to_string());
    }
    Ok(())
}

fn public_host(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => {
            host.contains('.')
                && host != "localhost"
                && !host.ends_with(".localhost")
                && !host.ends_with(".local")
                && !host.ends_with(".internal")
        }
        Some(url::Host::Ipv4(ip)) => {
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && ip.octets()[0] != 0
        }
        Some(url::Host::Ipv6(ip)) => {
            !ip.is_loopback()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && (ip.segments()[0] & 0xfe00) != 0xfc00
                && (ip.segments()[0] & 0xffc0) != 0xfe80
        }
        None => false,
    }
}

#[cfg(test)]
#[path = "../test/media.test.rs"]
mod tests;
