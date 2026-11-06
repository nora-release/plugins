use crate::{
    auth,
    definition::{Definition, ModelBase},
    model_catalog, models,
};
use nora_ai::{HttpClient, HttpMethod, HttpRequest, Model, ModelsCatalog, ResolvedProviderAuth};
use nora_provider_plugin_sdk::{NetworkProxy, PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;
use url::Url;

#[path = "decision_discovery.rs"]
mod decisions;

#[path = "discovery_parse.rs"]
pub mod parsing;

const MAX_PAGES: usize = 20;
const MAX_MODELS: usize = 4096;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    OpenAI,
    Anthropic,
    Google,
    Azure,
}

struct RemoteModels {
    models: Vec<Model>,
    supported_apis: BTreeMap<String, Vec<String>>,
    failed_parts: Vec<String>,
}

#[derive(Clone, Debug)]
struct ModelList {
    models: Vec<Model>,
    supported_apis: BTreeMap<String, Vec<String>>,
}

pub async fn discover(
    definition: &Definition,
    credentials: Option<&Value>,
    data_directory: &Path,
    client: &dyn HttpClient,
) -> PluginResult<Value> {
    discover_with_network(definition, credentials, data_directory, client, None).await
}

pub async fn discover_with_network(
    definition: &Definition,
    credentials: Option<&Value>,
    data_directory: &Path,
    client: &dyn HttpClient,
    proxy: Option<&NetworkProxy>,
) -> PluginResult<Value> {
    let explicit =
        credentials.is_some_and(auth::is_api) && !definition.config.models_url.trim().is_empty();
    let remote = credentials.is_some()
        && (explicit || definition.adapter.remote_models(credentials))
        && (explicit
            || !definition
                .config
                .model_discovery
                .as_ref()
                .is_some_and(|discovery| discovery.no_list));
    let catalog = if remote {
        model_catalog::cached(data_directory)
    } else {
        model_catalog::load(client, data_directory).await
    };
    let catalog_error = catalog.as_ref().err().map(|error| error.message.clone());
    let catalog = catalog.unwrap_or_default();
    let fallback = || preset(definition, credentials, &catalog, catalog_error.as_deref());
    let Some(value) = credentials else {
        return fallback();
    };
    if !explicit && !definition.adapter.remote_models(credentials) {
        return fallback();
    }
    if !explicit
        && definition
            .config
            .model_discovery
            .as_ref()
            .is_some_and(|discovery| discovery.no_list)
    {
        return fallback();
    }
    let remote = async {
        let (_, resolved) = auth::resolve_with_network(definition, value, proxy).await?;
        fetch_routes(definition, value, &resolved, &catalog, client).await
    };
    match remote.await {
        Ok(mut remote) => {
            supplement_models(definition, credentials, &mut remote.models);
            let empty = remote.models.is_empty() && remote.failed_parts.is_empty();
            let mut result = json!({"models":remote.models,"supportedAPIs":remote.supported_apis,"source":"remote","allowUnknownModels":false});
            if empty {
                result["authoritativeEmpty"] = json!(true);
            }
            if !remote.failed_parts.is_empty() {
                result["failedModelParts"] = json!(remote.failed_parts);
                result["modelError"] = json!("Some provider model lists could not be refreshed. The last successful lists were kept.");
            }
            add_default_route(&mut result, definition, credentials);
            Ok(result)
        }
        Err(error) => {
            let mut result = fallback()?;
            result["modelError"] = json!(error.message);
            Ok(result)
        }
    }
}

fn preset(
    definition: &Definition,
    credentials: Option<&Value>,
    catalog: &ModelsCatalog,
    error: Option<&str>,
) -> PluginResult<Value> {
    let mut models = models::catalog_models_with_catalog(definition, credentials, catalog)
        .map_err(PluginError::invalid)?;
    let source = if let Some(fallback) =
        definition
            .adapter
            .fallback_models(definition, credentials, catalog)
    {
        models = fallback;
        "preset"
    } else if definition
        .config
        .model_discovery
        .as_ref()
        .is_some_and(|discovery| discovery.no_list)
    {
        "preset"
    } else {
        "catalog"
    };
    supplement_models(definition, credentials, &mut models);
    let mut result = json!({"models":models,"source":source,"allowUnknownModels":true});
    if let Some(error) = error.filter(|_| result["models"].as_array().is_none_or(Vec::is_empty)) {
        result["modelError"] = json!(error);
    }
    add_default_route(&mut result, definition, credentials);
    Ok(result)
}

pub(crate) fn supplement_models(
    definition: &Definition,
    credentials: Option<&Value>,
    models: &mut Vec<Model>,
) {
    decisions::supplement(definition, credentials, models);
    for mut model in definition
        .adapter
        .supplemental_models(definition, credentials)
    {
        if models.iter().any(|existing| existing.id == model.id) {
            continue;
        }
        model.info.catalog_source = Some("preset".into());
        models.push(model);
    }
}

fn add_default_route(result: &mut Value, definition: &Definition, credentials: Option<&Value>) {
    let mut route = Model::new("", &definition.api, &definition.id);
    models::configure_model(definition, credentials, &mut route);
    result["defaultAPI"] = json!(route.api);
    result["defaultBaseURL"] = json!(route.base_url);
    let transports = definition
        .adapter
        .selectable_model_apis(credentials)
        .iter()
        .filter_map(|api| {
            definition
                .adapter
                .model_api(credentials, api)
                .map(|transport| (*api, transport))
        })
        .collect::<BTreeMap<_, _>>();
    if !transports.is_empty() {
        result["apiTransports"] = json!(transports);
    }
    result["apiEndpoints"] = json!(configured_bases(definition, credentials)
        .into_iter()
        .filter(|base| !base.base_url.is_empty())
        .map(|base| (base.api, base.base_url))
        .collect::<BTreeMap<_, _>>());
    if definition.config.decision_discovery.is_some() {
        result["apiEndpoints"]["systemone"] =
            json!(models::effective_endpoint(definition, credentials));
    }
    let templates=definition.config.model_routes.iter().filter_map(|rule| {
        let base=definition.config.model_discovery.as_ref()?.bases.iter().find(|base|base.api==rule.api)?;
        Some(json!({"pattern":rule.pattern,"model":{"api":base.api,"baseUrl":base.base_url,"contextWindow":0,"maxTokens":0}}))
    }).collect::<Vec<_>>();
    if !templates.is_empty() {
        result["modelTemplates"] = json!(templates);
    }
}

pub fn configured_bases(definition: &Definition, credentials: Option<&Value>) -> Vec<ModelBase> {
    if credentials.is_some_and(|value| !auth::is_api(value)) {
        return vec![ModelBase {
            api: definition.api.clone(),
            base_url: models::effective_endpoint(definition, credentials),
        }];
    }
    if let Some(discovery) = &definition.config.model_discovery {
        if definition.base_url.trim_end_matches('/')
            == discovery.default_base_url.trim_end_matches('/')
            && !discovery.bases.is_empty()
        {
            return discovery.bases.clone();
        }
    }
    vec![ModelBase {
        api: definition.api.clone(),
        base_url: models::effective_endpoint(definition, credentials),
    }]
}

pub(crate) fn resolved_bases(
    definition: &Definition,
    credentials: &Value,
    resolved: &ResolvedProviderAuth,
) -> Vec<ModelBase> {
    let mut bases = configured_bases(definition, Some(credentials));
    if let Some(endpoint) = definition.adapter.live_endpoint(credentials, resolved) {
        for base in &mut bases {
            base.base_url = endpoint.clone();
        }
    }
    bases
}

fn kind(_definition: &Definition, _credentials: &Value, api: &str) -> Option<Kind> {
    match api {
        "anthropic-messages" => Some(Kind::Anthropic),
        "google-generative-ai" => Some(Kind::Google),
        "azure-openai-responses" => Some(Kind::Azure),
        "openai-responses" | "openai-completions" => Some(Kind::OpenAI),
        _ => None,
    }
}

pub fn validated_url(base: &str) -> PluginResult<Url> {
    let mut url = Url::parse(base)
        .map_err(|_| PluginError::invalid("The provider model list address is invalid."))?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(PluginError::invalid(
            "The provider model list address is invalid.",
        ));
    }
    url.set_fragment(None);
    Ok(url)
}

// The candidate order is Magpie catalog/live.go's listURLs: a versioned
// endpoint is already complete; an Anthropic base may share a root list.
fn list_urls(base: &str, kind: Kind) -> PluginResult<Vec<Url>> {
    let base = validated_url(base)?;
    let path = base.path().trim_end_matches('/');
    let mut paths = Vec::new();
    match kind {
        Kind::Google => {
            let versioned = if versioned(path) {
                path.to_string()
            } else {
                format!("{path}/v1beta")
            };
            paths.push(format!("{versioned}/models"));
        }
        Kind::Azure => {
            let root = path.split("/openai").next().unwrap_or_default();
            paths.push(format!("{root}/openai/deployments"));
            paths.push(format!("{root}/openai/v1/models"));
        }
        _ => {
            paths.push(format!("{path}/models"));
            if kind == Kind::Anthropic || !versioned(path) {
                paths.push(format!("{path}/v1/models"));
                let mut root = path;
                for suffix in [
                    "/anthropic",
                    "/apps/anthropic",
                    "/api/anthropic",
                    "/v1",
                    "/api",
                    "/api/v1",
                ] {
                    root = root.strip_suffix(suffix).unwrap_or(root);
                }
                paths.push(format!("{root}/v1/models"));
                paths.push(format!("{root}/models"));
            }
        }
    }
    let mut seen = BTreeSet::new();
    Ok(paths
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .map(|path| {
            let mut url = base.clone();
            url.set_path(&path);
            if kind == Kind::Google {
                url.query_pairs_mut().append_pair("pageSize", "1000");
            }
            if kind == Kind::Azure && path.ends_with("/deployments") {
                url.set_query(Some("api-version=2022-12-01"));
            }
            url
        })
        .collect())
}

fn versioned(path: &str) -> bool {
    path.split('/').any(|segment| {
        let bytes = segment.as_bytes();
        bytes.len() >= 2 && matches!(bytes[0], b'v' | b'V') && bytes[1].is_ascii_digit()
    })
}

fn signed_request(
    mut url: Url,
    definition: &Definition,
    resolved: &ResolvedProviderAuth,
    kind: Kind,
    only_auth: Option<&str>,
    api_key: bool,
) -> HttpRequest {
    let token = resolved.token.as_deref().unwrap_or_default().trim();
    let mut headers = BTreeMap::<String, String>::from([
        ("accept".into(), "application/json".into()),
        ("user-agent".into(), "Nora".into()),
    ]);
    if !token.is_empty() {
        match kind {
            Kind::Google => {
                headers.insert("x-goog-api-key".into(), token.into());
            }
            Kind::Azure => {
                headers.insert("api-key".into(), token.into());
            }
            Kind::Anthropic
                if !crate::auth_headers::relay(definition, "anthropic-messages", url.as_str()) =>
            {
                headers.insert("x-api-key".into(), token.into());
            }
            _ if definition.adapter.bearer_model_list(resolved) => {
                headers.insert("authorization".into(), format!("Bearer {token}"));
            }
            _ => {
                if only_auth != Some("x-api-key") {
                    headers.insert("authorization".into(), format!("Bearer {token}"));
                }
                if only_auth != Some("bearer") {
                    headers.insert("x-api-key".into(), token.into());
                }
            }
        }
    }
    if kind == Kind::Anthropic {
        headers.insert("anthropic-version".into(), "2023-06-01".into());
    }
    definition
        .adapter
        .model_list_headers(&url, resolved, &mut headers);
    if let Some(scheme) = definition.config.api_key_auth.as_ref().filter(|_| api_key) {
        headers.retain(|name, _| {
            !["authorization", "x-api-key", "api-key", "x-goog-api-key"]
                .iter()
                .any(|known| name.eq_ignore_ascii_case(known))
        });
        let mut configured = resolved.clone();
        configured.scheme = scheme.clone();
        nora_ai::auth_headers::apply_resolved_auth_with_merge(
            &configured,
            &mut headers,
            |headers, name, value| {
                headers.retain(|key, _| !key.eq_ignore_ascii_case(&name));
                headers.insert(name, value);
            },
        );
        if let nora_ai::AuthScheme::QueryKey { name } = scheme {
            url.query_pairs_mut().append_pair(name, token);
        }
    }
    crate::adapter::merge_headers(&mut headers, resolved.headers.clone());
    crate::adapter::merge_headers(&mut headers, definition.config.headers.clone());
    if definition.config.models_url.trim().is_empty() || !definition.config.query_params.is_empty()
    {
        url.query_pairs_mut()
            .extend_pairs(&definition.config.query_params);
    }
    HttpRequest {
        method: HttpMethod::Get,
        url: url.into(),
        headers,
        body: None,
    }
}

async fn fetch_routes(
    definition: &Definition,
    credentials: &Value,
    resolved: &ResolvedProviderAuth,
    catalog: &ModelsCatalog,
    client: &dyn HttpClient,
) -> PluginResult<RemoteModels> {
    fetch_routes_with_budget(
        definition,
        credentials,
        resolved,
        catalog,
        client,
        Duration::from_secs(30),
    )
    .await
}

async fn fetch_routes_with_budget(
    definition: &Definition,
    credentials: &Value,
    resolved: &ResolvedProviderAuth,
    catalog: &ModelsCatalog,
    client: &dyn HttpClient,
    budget: Duration,
) -> PluginResult<RemoteModels> {
    let deadline = tokio::time::Instant::now() + budget;
    let known = models::catalog_models_with_catalog(definition, Some(credentials), catalog)
        .map_err(PluginError::invalid)?
        .into_iter()
        .map(|model| (model.id.clone(), model))
        .collect::<BTreeMap<_, _>>();
    let mut all = BTreeMap::<String, Model>::new();
    let mut order = Vec::new();
    let mut listed = BTreeMap::<String, ModelList>::new();
    let mut supported_apis = BTreeMap::<String, Vec<String>>::new();
    let mut asked = BTreeSet::new();
    let mut failed_parts = Vec::new();
    let mut last_error = PluginError::transport("This provider has no model list endpoint.");
    let explicit = auth::is_api(credentials) && !definition.config.models_url.trim().is_empty();
    let mut bases = resolved_bases(definition, credentials, resolved);
    if explicit {
        let base = bases
            .iter()
            .find(|base| base.api == definition.api)
            .cloned()
            .or_else(|| bases.first().cloned());
        bases = base.into_iter().collect();
    }
    for base in bases {
        let Some(kind) = kind(definition, credentials, &base.api) else {
            continue;
        };
        let urls = if explicit {
            vec![validated_url(definition.config.models_url.trim())?]
        } else {
            match definition
                .adapter
                .model_urls(&base.base_url, credentials)
                .await
            {
                Ok(Some(urls)) => urls,
                Ok(None) => match list_urls(&base.base_url, kind) {
                    Ok(urls) => urls,
                    Err(error) => {
                        last_error = error;
                        failed_parts.push(format!("{}|{}", base.api, base.base_url));
                        continue;
                    }
                },
                Err(error) => {
                    last_error = error;
                    failed_parts.push(format!("{}|{}", base.api, base.base_url));
                    continue;
                }
            }
        };
        let mut current = urls
            .iter()
            .find_map(|url| listed.get(url.as_str()).cloned());
        let mut attempted = false;
        if current.is_none() {
            for url in urls {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    attempted = true;
                    last_error =
                        PluginError::transport("The provider model list request timed out.");
                    break;
                }
                let request_id = format!("{kind:?}|{url}");
                if !asked.insert(request_id) {
                    continue;
                }
                attempted = true;
                let result = tokio::time::timeout(
                    Duration::from_secs(8).min(remaining),
                    fetch_pages(
                        url.clone(),
                        definition,
                        credentials,
                        resolved,
                        kind,
                        &known,
                        client,
                    ),
                )
                .await;
                let result = result
                    .map_err(|_| {
                        PluginError::transport("The provider model list request timed out.")
                    })
                    .and_then(|result| result);
                match result {
                    Ok(mut models) => {
                        for model in &mut models.models {
                            // Local services identify decisions in their own metadata;
                            // a cached catalog's label must not override that live list.
                            if decisions::uses_remote_metadata(definition)
                                && model.info.decision == Some(true)
                            {
                                model.info.decision = None;
                                model.info.chat = None;
                                model.info.tool_calling = None;
                            }
                            if !definition.adapter.preserve_model_api(credentials) {
                                model.api = base.api.clone();
                            }
                            model.base_url = if kind == Kind::Azure && !explicit {
                                let mut endpoint = url.clone();
                                endpoint.set_path(
                                    endpoint
                                        .path()
                                        .split("/openai")
                                        .next()
                                        .unwrap_or_default()
                                        .to_string()
                                        .as_str(),
                                );
                                endpoint.set_query(None);
                                format!("{}/openai/v1", endpoint.as_str().trim_end_matches('/'))
                            } else {
                                base.base_url.clone()
                            };
                            crate::profiles::route_model(definition, model);
                        }
                        models
                            .models
                            .retain(|model| crate::profiles::retain_model(definition, model));
                        listed.insert(url.to_string(), models.clone());
                        current = Some(models);
                        break;
                    }
                    Err(error) => last_error = error,
                }
            }
        }
        let Some(current) = current else {
            if attempted {
                failed_parts.push(format!("{}|{}", base.api, base.base_url));
            }
            continue;
        };
        for (id, apis) in current.supported_apis {
            let known = supported_apis.entry(id).or_default();
            for api in apis {
                if !known.contains(&api) {
                    known.push(api);
                }
            }
        }
        for model in current.models {
            if let Some(previous) = all.get_mut(&model.id) {
                if !model.input.contains(&nora_ai::InputModality::Image) {
                    previous
                        .input
                        .retain(|input| *input != nora_ai::InputModality::Image);
                }
            } else {
                order.push(model.id.clone());
                all.insert(model.id.clone(), model);
            }
        }
    }
    let mut decisions_refreshed = false;
    if let Some(result) =
        decisions::fetch(definition, credentials, resolved, client, deadline).await
    {
        match result {
            Ok(decisions) => {
                decisions_refreshed = true;
                for model in decisions {
                    if !all.contains_key(&model.id) {
                        order.push(model.id.clone());
                    }
                    supported_apis.remove(&model.id);
                    all.insert(model.id.clone(), model);
                }
            }
            Err(error) => {
                last_error = error;
                failed_parts.push(format!(
                    "systemone|{}",
                    models::effective_endpoint(definition, Some(credentials))
                ));
            }
        }
    }
    if all.is_empty()
        && !decisions_refreshed
        && (listed.is_empty()
            || definition
                .adapter
                .supplemental_models(definition, Some(credentials))
                .is_empty())
    {
        return Err(last_error);
    }
    let mut models = order
        .into_iter()
        .filter_map(|id| all.remove(&id))
        .collect::<Vec<_>>();
    decisions::supplement(definition, Some(credentials), &mut models);
    supported_apis.retain(|id, _| {
        !models
            .iter()
            .any(|model| model.id == *id && model.api == "systemone")
    });
    Ok(RemoteModels {
        models,
        supported_apis,
        failed_parts,
    })
}

async fn fetch_pages(
    mut url: Url,
    definition: &Definition,
    credentials: &Value,
    resolved: &ResolvedProviderAuth,
    kind: Kind,
    known: &BTreeMap<String, Model>,
    client: &dyn HttpClient,
) -> PluginResult<ModelList> {
    let mut models = Vec::new();
    let mut supported_apis = BTreeMap::new();
    let mut seen_ids = BTreeSet::new();
    let mut seen_pages = BTreeSet::new();
    let mut only_auth = None;
    for _ in 0..MAX_PAGES {
        if !seen_pages.insert(url.to_string()) {
            return Err(PluginError::transport(
                "The provider returned a repeated model list page.",
            ));
        }
        let payload = request_json(
            client,
            url.clone(),
            definition,
            credentials,
            resolved,
            kind,
            &mut only_auth,
        )
        .await?;
        let mut entries = parsing::entries(&payload, kind)?.iter().collect::<Vec<_>>();
        entries.sort_by_key(|entry| definition.adapter.model_priority(entry, credentials));
        for entry in entries {
            if let Some(model) = parsing::model(entry, kind, definition, credentials, known) {
                let apis = parsing::supported_apis(entry);
                let variants = definition.adapter.expand_model(model, entry, credentials);
                for model in variants {
                    if seen_ids.insert(model.id.clone()) {
                        if !apis.is_empty() {
                            supported_apis.insert(model.id.clone(), apis.clone());
                        }
                        models.push(model);
                    }
                }
            }
            if models.len() > MAX_MODELS {
                return Err(PluginError::transport(
                    "The provider returned too many models.",
                ));
            }
        }
        let Some((key, cursor)) = next_page(&payload, kind)? else {
            if models.is_empty()
                && !decisions::uses_remote_metadata(definition)
                && definition
                    .adapter
                    .supplemental_models(definition, Some(credentials))
                    .is_empty()
            {
                return Err(PluginError::transport(
                    "The provider did not return any available chat models.",
                ));
            }
            return Ok(ModelList {
                models,
                supported_apis,
            });
        };
        let pairs = url
            .query_pairs()
            .filter(|(name, _)| name != key && name != "limit")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect::<Vec<_>>();
        url.set_query(None);
        url.query_pairs_mut()
            .extend_pairs(pairs)
            .append_pair(key, &cursor);
        if kind != Kind::Google {
            url.query_pairs_mut().append_pair("limit", "1000");
        }
    }
    Err(PluginError::transport(
        "The provider model list exceeded the page limit.",
    ))
}

fn next_page(payload: &Value, kind: Kind) -> PluginResult<Option<(&'static str, String)>> {
    if kind == Kind::Google {
        return Ok(payload
            .get("nextPageToken")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .map(|token| ("pageToken", token.into())));
    }
    if payload.get("has_more").and_then(Value::as_bool) == Some(true) {
        let cursor = payload
            .get("last_id")
            .and_then(Value::as_str)
            .filter(|cursor| !cursor.is_empty())
            .ok_or_else(|| {
                PluginError::transport("The provider model list is missing its next page cursor.")
            })?;
        return Ok(Some(("after_id", cursor.into())));
    }
    Ok(None)
}

async fn request_json(
    client: &dyn HttpClient,
    url: Url,
    definition: &Definition,
    credentials: &Value,
    resolved: &ResolvedProviderAuth,
    kind: Kind,
    only_auth: &mut Option<&'static str>,
) -> PluginResult<Value> {
    let api = if kind == Kind::Anthropic {
        "anthropic-messages"
    } else {
        "openai-completions"
    };
    if only_auth.is_none() && crate::auth_headers::relay(definition, api, url.as_str()) {
        *only_auth = crate::auth_headers::choice(definition, credentials, url.as_str());
    }
    let mut request = signed_request(
        url,
        definition,
        resolved,
        kind,
        *only_auth,
        auth::is_api(credentials),
    );
    crate::adapter::merge_headers(
        &mut request.headers,
        crate::account_headers::read(credentials)?,
    );
    let response = crate::auth_headers::send_with(
        definition,
        credentials,
        api,
        request,
        only_auth,
        |request| async move {
            client.stream(request).await.map_err(|_| {
                PluginError::transport("The provider model list could not be reached.")
            })
        },
    )
    .await?;
    let status = response.status;
    let body = nora_ai::collect_http_body_text_limited(response.body, MAX_RESPONSE_BYTES)
        .await
        .map_err(|_| {
            PluginError::transport("The provider model list response stopped before it finished.")
        })?;
    if matches!(status, 401 | 403) {
        return Err(PluginError::authentication(format!(
            "The provider denied access to its model list (HTTP {status})."
        )));
    }
    if !(200..300).contains(&status) {
        return Err(PluginError::transport(format!(
            "The provider model list returned HTTP {status}."
        )));
    }
    serde_json::from_str(&body)
        .map_err(|_| PluginError::transport("The provider returned an invalid model list."))
}

#[cfg(test)]
#[path = "../test/discovery.test.rs"]
mod tests;
