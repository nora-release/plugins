use crate::{definition::Definition, transport};
use futures::StreamExt;
use nora_ai::{HttpClientError, HttpStreamingResponse};
use nora_provider_plugin_sdk::{FetchRequest, FetchResponse, PluginError, PluginResult};
use std::collections::BTreeMap;

pub(crate) async fn fetch(
    definition: &Definition,
    mut request: FetchRequest,
) -> PluginResult<FetchResponse> {
    if request.body.is_some() {
        return Err(PluginError::invalid(
            "Provide one provider request body source.",
        ));
    }
    if definition.config.endpoint_kind.as_deref() == Some("region") {
        return Err(PluginError::unsupported(
            "multipart uploads for this provider protocol",
        ));
    }
    let file = request
        .body_file
        .take()
        .ok_or_else(|| PluginError::invalid("A provider upload file is required."))?;
    let credentials = request
        .credentials
        .clone()
        .ok_or_else(|| PluginError::authentication("Connect a provider account."))?;
    let proxy = request.network_proxy.clone();
    let outgoing = transport::build_request(definition, request).await?;
    let client = crate::network_client::raw_for_url(
        definition,
        proxy.as_ref(),
        crate::auth::is_api(&credentials),
        &outgoing.url,
    )?;
    if outgoing.body.is_some() {
        return Err(PluginError::unsupported(
            "streaming uploads that require a rewritten provider request",
        ));
    }
    let secrets = transport::request_secrets(&outgoing);
    let api = transport::request_api(definition, &credentials, &outgoing.url);
    let response = crate::auth_headers::send_with(
        definition,
        &credentials,
        &api,
        outgoing,
        &mut None,
        |outgoing| {
            let client = client.clone();
            let file = file.clone();
            async move {
                let method = reqwest::Method::from_bytes(outgoing.method.as_str().as_bytes())
                    .map_err(|_| PluginError::invalid("The provider HTTP method is invalid."))?;
                let mut builder = client.request(method, &outgoing.url);
                for (name, value) in outgoing.headers {
                    builder = builder.header(&name, &value);
                }
                let response = nora_provider_plugin_sdk::send_http(
                    builder
                        .header(reqwest::header::CONTENT_LENGTH, file.length)
                        .body(file.http_body().await?),
                )
                .await
                .map_err(|_| {
                    PluginError::transport("The provider upload could not be completed.")
                })?;
                let status = response.status().as_u16();
                let headers: BTreeMap<String, String> = response
                    .headers()
                    .iter()
                    .filter_map(|(name, value)| {
                        value
                            .to_str()
                            .ok()
                            .map(|value| (name.to_string(), value.into()))
                    })
                    .collect();
                Ok(HttpStreamingResponse {
                    status,
                    headers,
                    body: Box::pin(response.bytes_stream().map(|chunk| {
                        chunk.map_err(|_| {
                            HttpClientError::Transport(
                                "The provider upload response stopped.".into(),
                            )
                        })
                    })),
                })
            }
        },
    )
    .await?;
    transport::finish_response(response, secrets).await
}

#[cfg(test)]
#[path = "../test/transport_file.test.rs"]
mod tests;
