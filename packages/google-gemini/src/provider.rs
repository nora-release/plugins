use async_trait::async_trait;
use futures::StreamExt;
use nora_ai::{HttpClient, HttpRequest, Model};
use nora_provider::{definition::Definition, transport, ProviderAdapter};
use nora_provider_plugin_sdk::{FetchRequest, FetchResponse, PluginError, PluginResult};
use serde_json::Value;
use std::collections::BTreeMap;
use url::Url;

pub const MAX_RESPONSE_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Default)]
pub struct GoogleGemini;

#[async_trait]
impl ProviderAdapter for GoogleGemini {
    fn id(&self) -> Option<&'static str> {
        Some("google-gemini")
    }

    fn enrich_model(&self, entry: &Value, model: &mut Model, _credentials: &Value) {
        let methods = entry
            .get("supportedGenerationMethods")
            .and_then(Value::as_array);
        if methods.is_some_and(|methods| {
            methods.iter().any(|method| {
                matches!(method.as_str(), Some("embedContent" | "batchEmbedContents"))
            })
        }) {
            model.info.embedding = Some(true);
            if !methods.is_some_and(|methods| {
                methods
                    .iter()
                    .any(|method| method.as_str() == Some("generateContent"))
            }) {
                model.info.chat = Some(false);
                model.info.tool_calling = Some(false);
            }
        }
    }

    async fn fetch_override(
        &self,
        definition: &Definition,
        request: &FetchRequest,
        client: &dyn HttpClient,
    ) -> PluginResult<Option<FetchResponse>> {
        let target = Url::parse(&request.url)
            .map_err(|_| PluginError::invalid("The Google request address is invalid."))?;
        let (suffix, kind) = if target.path().ends_with("/images/generations") {
            ("/images/generations", "image")
        } else if target.path().ends_with("/images/edits") {
            ("/images/edits", "edit")
        } else if target.path().ends_with("/embeddings") {
            ("/embeddings", "embedding")
        } else {
            return Ok(None);
        };
        if !request.method.eq_ignore_ascii_case("POST") {
            return Err(PluginError::invalid(
                "Google media operations require POST.",
            ));
        }
        // Authenticate and validate the original configured endpoint before any
        // caller-provided reference image is fetched or a native path is built.
        let mut empty = request.clone();
        empty.body = None;
        empty.body_file = None;
        let mut prepared = transport::build_request(definition, empty).await?;
        let mut target = Url::parse(&prepared.url)
            .map_err(|_| PluginError::invalid("The Google endpoint is invalid."))?;
        let mut base = target
            .path()
            .trim_end_matches(suffix)
            .trim_end_matches('/')
            .trim_end_matches("/openai")
            .to_owned();
        if !["/v1beta", "/v1", "/v1alpha"]
            .iter()
            .any(|version| base.ends_with(version))
        {
            base.push_str("/v1beta");
        }
        target.set_path(&base);
        let mut query = Vec::new();
        for (key, value) in target.query_pairs() {
            if key == "key" {
                prepared
                    .headers
                    .retain(|key, _| !key.eq_ignore_ascii_case("x-goog-api-key"));
                prepared
                    .headers
                    .insert("x-goog-api-key".into(), value.into_owned());
            } else {
                query.push((key.into_owned(), value.into_owned()));
            }
        }
        target.set_query(None);
        if !query.is_empty() {
            target.query_pairs_mut().extend_pairs(query);
        }
        prepared.url = target.into();
        prepared.headers.retain(|key, _| {
            !key.eq_ignore_ascii_case("content-type") && !key.eq_ignore_ascii_case("accept")
        });
        prepared
            .headers
            .insert("content-type".into(), "application/json".into());
        prepared
            .headers
            .insert("accept".into(), "application/json".into());
        let body = nora_provider::media_upload::parse(request).await?;
        let response = if kind == "embedding" {
            crate::embeddings::fetch(prepared, body, client).await?
        } else {
            crate::image::fetch(prepared, body, kind == "edit", client).await?
        };
        Ok(Some(response))
    }
}

pub enum JsonResponse {
    Value(Value, BTreeMap<String, String>),
    Error(FetchResponse),
}

pub async fn send_json(
    client: &dyn HttpClient,
    request: HttpRequest,
) -> PluginResult<JsonResponse> {
    let secrets = transport::request_secrets(&request);
    let mut response = client
        .stream(request)
        .await
        .map_err(|_| PluginError::transport("The Google media endpoint could not be reached."))?;
    if response.status >= 300 {
        return transport::finish_response(response, secrets)
            .await
            .map(JsonResponse::Error);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.body.next().await {
        let chunk =
            chunk.map_err(|_| PluginError::transport("The Google media response stopped."))?;
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(PluginError::transport(
                "The Google media response exceeds 256 MiB.",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    let value = serde_json::from_slice(&bytes)
        .map_err(|_| PluginError::transport("Google returned an invalid media response."))?;
    let headers = response
        .headers
        .into_iter()
        .map(|(key, value)| (key, transport::redact(value, &secrets)))
        .collect();
    Ok(JsonResponse::Value(value, headers))
}

pub fn json_response(
    mut headers: BTreeMap<String, String>,
    value: Value,
) -> PluginResult<FetchResponse> {
    headers.retain(|key, _| {
        !matches!(
            key.to_ascii_lowercase().as_str(),
            "content-length" | "content-encoding" | "transfer-encoding" | "content-type"
        )
    });
    headers.insert("content-type".into(), "application/json".into());
    Ok(FetchResponse::bytes(
        200,
        headers,
        serde_json::to_vec(&value).map_err(|_| {
            PluginError::transport("The Google media response could not be encoded.")
        })?,
    ))
}

pub fn native_request(
    mut template: HttpRequest,
    model: &str,
    method: &str,
    body: Value,
) -> PluginResult<HttpRequest> {
    let model = model.strip_prefix("models/").unwrap_or(model);
    if model.is_empty()
        || model.len() > 512
        || !model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(PluginError::invalid("The Google model ID is invalid."));
    }
    let mut url = Url::parse(&template.url)
        .map_err(|_| PluginError::invalid("The Google endpoint is invalid."))?;
    url.set_path(&format!(
        "{}/models/{model}:{method}",
        url.path().trim_end_matches('/')
    ));
    template.url = url.into();
    template.body = Some(
        serde_json::to_vec(&body)
            .map_err(|_| PluginError::invalid("The Google media request could not be encoded."))?,
    );
    Ok(template)
}

#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
