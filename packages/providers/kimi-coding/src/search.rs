use nora_ai::{HttpClient, HttpMethod, HttpRequest, ResolvedProviderAuth};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::{collections::BTreeMap, time::Duration};

pub(crate) async fn search(
    auth: &ResolvedProviderAuth,
    query: &str,
    client: &dyn HttpClient,
) -> PluginResult<Value> {
    let base = auth
        .base_url
        .as_deref()
        .ok_or_else(|| PluginError::invalid("Kimi Code needs its coding endpoint."))?;
    let url = url::Url::parse(base)
        .map_err(|_| PluginError::invalid("The Kimi Code endpoint is invalid."))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(PluginError::invalid(
            "The Kimi Code search endpoint is invalid.",
        ));
    }
    let key = auth
        .token
        .as_deref()
        .filter(|key| !key.is_empty() && !key.contains(['\r', '\n']))
        .ok_or_else(|| PluginError::authentication("Connect a Kimi Code API key."))?;
    let base = base.trim_end_matches('/');
    let endpoint = format!(
        "{base}{}/search",
        if base.ends_with("/coding") { "/v1" } else { "" }
    );
    tokio::time::timeout(Duration::from_secs(60), async {
        let response = client.stream(HttpRequest {
            method: HttpMethod::Post, url: endpoint,
            headers: BTreeMap::from([("authorization".into(),format!("Bearer {key}")),("content-type".into(),"application/json".into()),("accept".into(),"application/json".into()),("user-agent".into(),"KimiCLI/1.52.0".into())]),
            body: Some(json!({"text_query":query,"limit":5,"enable_page_crawling":false,"timeout_seconds":30}).to_string().into_bytes()),
        }).await.map_err(|_| PluginError::transport("The Kimi Code search connection stopped."))?;
        let status = response.status;
        let body = nora_ai::collect_http_body_text_limited(response.body, 4*1024*1024).await.map_err(|_| PluginError::transport("Kimi Code returned an incomplete or oversized search reply."))?;
        if !(200..300).contains(&status) {
            return Err(if matches!(status,401|403) { PluginError::authentication("Kimi Code refused this search account.") } else { PluginError::transport(format!("Kimi Code search returned HTTP {status}.")) });
        }
        let value: Value=serde_json::from_str(&body).map_err(|_| PluginError::transport("Kimi Code returned invalid search JSON."))?;
        let results=value.get("search_results").and_then(Value::as_array).ok_or_else(|| PluginError::transport("Kimi Code returned no search results."))?;
        nora_provider_search_api::provider_results(results)
    }).await.map_err(|_| PluginError::transport("Kimi Code search timed out."))?
}

#[cfg(test)]
#[path = "../test/search.test.rs"]
mod tests;
