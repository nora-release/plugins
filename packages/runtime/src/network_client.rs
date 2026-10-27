use crate::definition::Definition;
use async_trait::async_trait;
use nora_ai::{HttpClient, HttpClientError, HttpRequest, HttpStreamingResponse, ReqwestHttpClient};
use nora_provider_plugin_sdk::{NetworkProxy, PluginError, PluginResult};
use std::collections::BTreeSet;

#[derive(Clone)]
pub(crate) struct Client {
    secure: ReqwestHttpClient,
    unchecked: Option<ReqwestHttpClient>,
    hosts: BTreeSet<(String, u16)>,
}

impl Client {
    pub(crate) fn new(
        definition: Option<&Definition>,
        proxy: Option<&NetworkProxy>,
        api: bool,
    ) -> PluginResult<Self> {
        let hosts = definition
            .filter(|definition| {
                api && definition.config.insecure_tls && takes_unchecked_tls(definition)
            })
            .map(unchecked_hosts)
            .unwrap_or_default();
        let unchecked = (!hosts.is_empty())
            .then(|| build(proxy, &hosts).map(observed))
            .transpose()?;
        Ok(Self {
            secure: observed(build(proxy, &BTreeSet::new())?),
            unchecked,
            hosts,
        })
    }
}

#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        let unchecked = host(&request.url).is_some_and(|host| self.hosts.contains(&host));
        match self.unchecked.as_ref().filter(|_| unchecked) {
            Some(client) => client.stream(request).await,
            None => self.secure.stream(request).await,
        }
    }
}

pub(crate) fn raw_for_url(
    definition: &Definition,
    proxy: Option<&NetworkProxy>,
    api: bool,
    target: &str,
) -> PluginResult<reqwest::Client> {
    let unchecked = api
        && definition.config.insecure_tls
        && takes_unchecked_tls(definition)
        && host(target).is_some_and(|host| unchecked_hosts(definition).contains(&host));
    let hosts = if unchecked {
        unchecked_hosts(definition)
    } else {
        BTreeSet::new()
    };
    build(proxy, &hosts)
}

pub(crate) fn takes_unchecked_tls(definition: &Definition) -> bool {
    definition.config.extra.get("customTemplate") == Some(&serde_json::Value::Bool(true))
        && (definition.auth_methods.is_empty()
            || definition.auth_methods.iter().any(|method| {
                matches!(
                    method.get("type").and_then(serde_json::Value::as_str),
                    Some("api" | "apiKey")
                )
            }))
        && definition.config.endpoint_kind.as_deref() != Some("region")
}

fn unchecked_hosts(definition: &Definition) -> BTreeSet<(String, u16)> {
    std::iter::once(definition.base_url.as_str())
        .chain(
            definition
                .config
                .model_discovery
                .iter()
                .flat_map(|discovery| discovery.bases.iter().map(|base| base.base_url.as_str())),
        )
        .filter_map(host)
        .collect()
}

fn host(raw: &str) -> Option<(String, u16)> {
    let url = url::Url::parse(raw).ok()?;
    if url.scheme() != "https" {
        return None;
    }
    Some((
        url.host_str()?.trim_end_matches('.').to_ascii_lowercase(),
        url.port_or_known_default()?,
    ))
}

fn build(
    proxy: Option<&NetworkProxy>,
    hosts: &BTreeSet<(String, u16)>,
) -> PluginResult<reqwest::Client> {
    let mut builder = nora_provider_network::http_client_builder(proxy)?;
    if !hosts.is_empty() {
        builder = builder.use_preconfigured_tls(crate::tls_verifier::configuration(
            hosts.iter().map(|(host, _)| host.clone()).collect(),
            proxy,
        )?);
    }
    builder
        .build()
        .map_err(|_| PluginError::transport("The provider HTTP client could not be initialized."))
}

fn observed(client: reqwest::Client) -> ReqwestHttpClient {
    ReqwestHttpClient::with_client(client).with_observer_fn(|| async {
        nora_provider_plugin_sdk::before_http_request()
            .await
            .map_err(|error| HttpClientError::Transport(error.to_string()))
    })
}

#[cfg(test)]
#[path = "../test/network_client.test.rs"]
mod tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "../test/system_trust.test.rs"]
mod system_trust_tests;
