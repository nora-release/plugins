use crate::{auth, definition::Definition, discovery};
use futures::StreamExt;
use nora_ai::{
    AuthScheme, AwsSigV4, AwsSigV4Credentials, HttpClient, HttpMethod, HttpRequest,
    HttpStreamingResponse,
};
use nora_provider_plugin_sdk::{
    FetchRequest, FetchResponse, NetworkProxy, PluginError, PluginResult,
};
use serde_json::Value;
use std::collections::BTreeMap;

pub fn http1_client(proxy: Option<&NetworkProxy>) -> PluginResult<reqwest::Client> {
    crate::network_client::http1_client(proxy)
}

#[cfg(test)]
pub(crate) async fn send(
    client: &dyn HttpClient,
    definition: &Definition,
    credentials: &Value,
    api: &str,
    request: HttpRequest,
) -> PluginResult<HttpStreamingResponse> {
    send_with_proxy(client, definition, credentials, api, request, None).await
}

pub(crate) async fn send_with_proxy(
    client: &dyn HttpClient,
    definition: &Definition,
    credentials: &Value,
    api: &str,
    request: HttpRequest,
    proxy: Option<&NetworkProxy>,
) -> PluginResult<HttpStreamingResponse> {
    #[derive(serde::Deserialize)]
    struct ModelName {
        model: String,
    }
    let observed_statuses = definition.adapter.observed_error_statuses(credentials);
    let model = if observed_statuses.is_empty() {
        None
    } else {
        request
            .body
            .as_deref()
            .and_then(|body| serde_json::from_slice::<ModelName>(body).ok())
            .map(|name| name.model)
            .filter(|id| !id.is_empty() && id.len() <= 512)
    };
    definition
        .adapter
        .before_send(definition, credentials, &request, client)
        .await?;
    let mut response = crate::auth_headers::send_with(
        definition,
        credentials,
        api,
        request,
        &mut None,
        |request| {
            definition
                .adapter
                .send_fetch(definition, credentials, request, client, proxy)
        },
    )
    .await?;
    definition
        .adapter
        .observe_response(credentials, response.status, &response.headers);
    if observed_statuses.contains(&response.status) {
        if let Some(model) = model {
            let body = crate::http_response::error_prefix(&mut response).await;
            definition
                .adapter
                .observe_response_error(credentials, &model, response.status, &body);
        }
    }
    Ok(response)
}

pub fn request_secrets(request: &HttpRequest) -> Vec<String> {
    let mut secrets = Vec::new();
    for (name, value) in &request.headers {
        let name = name.to_ascii_lowercase();
        if ["auth", "key", "token", "secret"]
            .iter()
            .any(|part| name.contains(part))
        {
            secrets.push(value.clone());
            if value
                .get(..7)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Bearer "))
            {
                secrets.push(value[7..].into());
            }
        }
    }
    if let Ok(url) = url::Url::parse(&request.url) {
        for (name, value) in url.query_pairs() {
            if ["key", "token", "secret"]
                .iter()
                .any(|part| name.to_ascii_lowercase().contains(part))
            {
                secrets.push(value.into_owned());
            }
        }
    }
    let escaped = secrets
        .iter()
        .map(|secret| url::form_urlencoded::byte_serialize(secret.as_bytes()).collect::<String>())
        .collect::<Vec<_>>();
    secrets.extend(escaped);
    secrets.retain(|secret| !secret.is_empty());
    secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    secrets.dedup();
    secrets
}

pub fn redact(mut value: String, secrets: &[String]) -> String {
    for secret in secrets {
        value = value.replace(secret, "[redacted]");
    }
    value
}

pub async fn finish_response(
    response: HttpStreamingResponse,
    secrets: Vec<String>,
) -> PluginResult<FetchResponse> {
    let mut headers = response
        .headers
        .into_iter()
        .map(|(name, value)| (name, redact(value, &secrets)))
        .collect::<BTreeMap<_, _>>();
    if response.status >= 400 {
        let body = nora_ai::collect_http_body_text_limited(response.body, 64 * 1024)
            .await
            .map_err(|_| PluginError::transport("The provider error response stopped."))?;
        headers.retain(|name, _| {
            !matches!(
                name.to_ascii_lowercase().as_str(),
                "content-length" | "content-encoding"
            )
        });
        return Ok(FetchResponse::bytes(
            response.status,
            headers,
            redact(body, &secrets).into_bytes(),
        ));
    }
    Ok(FetchResponse {
        status: response.status,
        headers,
        body: Box::pin(response.body.map(|chunk| {
            chunk
                .map(|bytes| bytes.to_vec())
                .map_err(|_| PluginError::transport("The provider stream stopped."))
        })),
    })
}

pub async fn build_request(
    definition: &Definition,
    request: FetchRequest,
) -> PluginResult<HttpRequest> {
    let value = request
        .credentials
        .as_ref()
        .ok_or_else(|| PluginError::authentication("Connect a provider account."))?;
    let (credentials, mut resolved) =
        auth::resolve_with_network(definition, value, request.network_proxy.as_ref()).await?;
    let mut target = url::Url::parse(&request.url)
        .map_err(|_| PluginError::invalid("The provider request address is invalid."))?;
    let endpoint = discovery::resolved_bases(definition, value, &resolved)
        .into_iter()
        .filter_map(|base| url::Url::parse(&base.base_url).ok())
        .find(|endpoint| {
            let base_path = endpoint.path().trim_end_matches('/');
            endpoint.origin() == target.origin()
                && (target.path() == base_path
                    || target.path().starts_with(&format!("{base_path}/")))
        });
    if !target.username().is_empty() || target.password().is_some() || endpoint.is_none() {
        return Err(PluginError::invalid(
            "The request address is outside this provider's configured endpoint.",
        ));
    }
    let endpoint = endpoint.expect("The configured endpoint was checked above");
    if auth::is_api(value) && definition.config.api_key_auth.is_none() {
        resolved.scheme = auth::api_key_scheme(&request_api(definition, value, target.as_str()));
    }
    let method = match request.method.to_ascii_uppercase().as_str() {
        "GET" => HttpMethod::Get,
        "POST" => HttpMethod::Post,
        "PUT" => HttpMethod::Put,
        "PATCH" => HttpMethod::Patch,
        "DELETE" => HttpMethod::Delete,
        _ => return Err(PluginError::invalid("The provider HTTP method is invalid.")),
    };
    let mut body = request.decode_body()?;
    let session = crate::extra_body::session(&request, body.as_deref());
    let constraints = request.constraints.clone();
    let mut headers = request
        .headers
        .into_iter()
        .filter(|(name, _)| {
            !matches!(
                name.to_ascii_lowercase().as_str(),
                "authorization"
                    | "x-api-key"
                    | "x-goog-api-key"
                    | "api-key"
                    | "host"
                    | "content-length"
                    | "connection"
                    | "transfer-encoding"
                    | "x-amz-date"
                    | "x-amz-security-token"
                    | "x-amz-content-sha256"
            )
        })
        .collect::<BTreeMap<_, _>>();
    crate::caller_user_agent::apply(
        definition,
        value,
        request.caller_user_agent.as_deref(),
        &mut headers,
    );
    headers.extend(definition.config.headers.clone());
    definition
        .adapter
        .prepare_fetch(
            definition,
            value,
            &mut target,
            &mut headers,
            &mut body,
            &mut resolved,
        )
        .await?;
    let api = request_api(definition, value, target.as_str());
    let gemini_thinking = (constraints.reasoning_disabled
        && matches!(
            api.as_str(),
            "google-generative-ai" | "google-gemini-cli" | "google-antigravity" | "google-vertex"
        ))
    .then(|| crate::request_constraints::gemini_thinking(body.as_deref()))
    .flatten();
    crate::extra_body::apply(definition, value, target.path(), &session, &mut body);
    crate::request_constraints::apply(&api, &constraints, gemini_thinking.as_ref(), &mut body)?;
    for (key, value) in &definition.config.query_params {
        target.query_pairs_mut().append_pair(key, value);
    }
    if definition.config.endpoint_kind.as_deref() == Some("region") {
        let aws = parse_aws(&credentials.access_token).ok_or_else(|| {
            PluginError::authentication(
                "Enter AWS credentials as accessKeyId:secretAccessKey[:sessionToken] or JSON.",
            )
        })?;
        let region = endpoint
            .host_str()
            .and_then(|host| host.strip_prefix("bedrock-runtime."))
            .and_then(|host| host.strip_suffix(".amazonaws.com"))
            .ok_or_else(|| PluginError::invalid("The AWS region is invalid."))?;
        headers = AwsSigV4::sign_post(
            target.as_str(),
            body.as_deref().unwrap_or_default(),
            region,
            "bedrock",
            &aws,
            headers,
        );
    } else {
        if let AuthScheme::QueryKey { name } = &resolved.scheme {
            let query = target
                .query_pairs()
                .filter(|(key, _)| key != name)
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect::<Vec<_>>();
            target.set_query(None);
            target.query_pairs_mut().extend_pairs(query);
            if let Some(token) = resolved.token.as_deref() {
                target.query_pairs_mut().append_pair(name, token);
            }
        }
        nora_ai::auth_headers::apply_resolved_auth_with_merge(
            &resolved,
            &mut headers,
            |headers, name, value| {
                headers.retain(|key, _| !key.eq_ignore_ascii_case(&name));
                headers.insert(name, value);
            },
        );
        let api = request_api(definition, value, target.as_str());
        crate::auth_headers::apply(
            definition,
            value,
            &api,
            target.as_str(),
            resolved.token.as_deref().unwrap_or_default(),
            &mut headers,
        );
        crate::adapter::merge_headers(&mut headers, definition.config.headers.clone());
        crate::adapter::merge_headers(&mut headers, crate::account_headers::read(value)?);
    }
    definition
        .adapter
        .finalize_fetch(value, &target, &mut headers, &mut body)?;
    Ok(HttpRequest {
        method,
        url: target.into(),
        headers,
        body,
    })
}

pub(crate) fn request_api(definition: &Definition, credentials: &Value, target: &str) -> String {
    let Ok(target) = url::Url::parse(target) else {
        return definition.api.clone();
    };
    if target.path().ends_with("/messages") || target.path().ends_with("/messages/count_tokens") {
        return "anthropic-messages".into();
    }
    if target.path().ends_with("/chat/completions") {
        return "openai-completions".into();
    }
    if target.path().ends_with("/systemone") {
        return "systemone".into();
    }
    if target.path().ends_with("/responses") {
        return "openai-responses".into();
    }
    discovery::configured_bases(definition, Some(credentials))
        .into_iter()
        .filter_map(|base| Some((base.api, url::Url::parse(&base.base_url).ok()?)))
        .filter(|(_, base)| {
            base.origin() == target.origin()
                && (target.path() == base.path().trim_end_matches('/')
                    || target
                        .path()
                        .starts_with(&format!("{}/", base.path().trim_end_matches('/'))))
        })
        .max_by_key(|(_, base)| base.path().len())
        .map(|(api, _)| api)
        .unwrap_or_else(|| definition.api.clone())
}

fn parse_aws(raw: &str) -> Option<AwsSigV4Credentials> {
    if let Ok(value) = serde_json::from_str::<Value>(raw) {
        return Some(AwsSigV4Credentials {
            access_key_id: value
                .get("accessKeyId")
                .or_else(|| value.get("AWS_ACCESS_KEY_ID"))
                .and_then(Value::as_str)?
                .into(),
            secret_access_key: value
                .get("secretAccessKey")
                .or_else(|| value.get("AWS_SECRET_ACCESS_KEY"))
                .and_then(Value::as_str)?
                .into(),
            session_token: value
                .get("sessionToken")
                .or_else(|| value.get("AWS_SESSION_TOKEN"))
                .and_then(Value::as_str)
                .map(str::to_string),
        });
    }
    let mut parts = raw.splitn(3, ':');
    let access = parts.next().filter(|value| !value.is_empty())?;
    let secret = parts.next().filter(|value| !value.is_empty())?;
    Some(AwsSigV4Credentials {
        access_key_id: access.into(),
        secret_access_key: secret.into(),
        session_token: parts
            .next()
            .filter(|value| !value.is_empty())
            .map(str::to_string),
    })
}

#[cfg(test)]
#[path = "../test/transport.test.rs"]
mod tests;
