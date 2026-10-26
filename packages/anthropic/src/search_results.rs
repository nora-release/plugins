use super::cli;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::BTreeSet;

#[derive(Clone)]
pub(crate) struct History {
    namespace: String,
    calls: BTreeSet<String>,
    completed: BTreeSet<String>,
}
impl Default for History {
    fn default() -> Self {
        Self {
            namespace: cli::random(),
            calls: BTreeSet::new(),
            completed: BTreeSet::new(),
        }
    }
}
pub(crate) struct Source {
    pub id: String,
    pub query: String,
    pub pages: Value,
}
impl History {
    pub fn call(&mut self, id: &str) -> PluginResult<()> {
        if self.calls.len() >= 10000 && !self.calls.contains(id) {
            return Err(PluginError::transport(
                "Claude Code returned too many search calls.",
            ));
        }
        self.calls.insert(id.to_owned());
        Ok(())
    }
    pub fn result(&mut self, event: &Value) -> PluginResult<Option<Source>> {
        let results = event.pointer("/message/content").and_then(Value::as_array);
        let ids = results
            .into_iter()
            .flatten()
            .filter(|part| part["type"] == "tool_result" && part["is_error"] != true)
            .filter_map(|part| part["tool_use_id"].as_str())
            .filter(|id| self.calls.contains(*id))
            .collect::<BTreeSet<_>>();
        if ids.is_empty() {
            return Ok(None);
        }
        if ids.len() != 1 {
            return Err(PluginError::transport(
                "Claude Code returned an ambiguous search result.",
            ));
        }
        let id = *ids.first().unwrap();
        if self.completed.contains(id) {
            return Ok(None);
        }
        let payload = &event["tool_use_result"];
        let Some(query) = cli::text(payload.get("query"), 16384) else {
            return Ok(None);
        };
        let Some(results) = payload["results"].as_array() else {
            return Ok(None);
        };
        if results.len() > 1000 {
            return Err(PluginError::transport(
                "Claude Code returned too many search results.",
            ));
        }
        let mut pages = Vec::new();
        let mut urls = BTreeSet::new();
        for result in results {
            for page in public_pages(&result["content"])? {
                if urls.insert(page["url"].as_str().unwrap().to_owned()) {
                    if pages.len() >= 1000 {
                        return Err(PluginError::transport(
                            "Claude Code returned too many source pages.",
                        ));
                    }
                    pages.push(page);
                }
            }
        }
        self.completed.insert(id.into());
        Ok(Some(Source {
            id: format!(
                "srvtoolu_{}",
                &cli::hash(&format!("{}\0{id}", self.namespace))[..32]
            ),
            query: query.to_owned(),
            pages: json!(pages),
        }))
    }
}

/// CLI metadata is not a wire response. Copy only public page fields, never
/// opaque provider data or authentication-bearing URLs into another request.
pub(crate) fn public_pages(value: &Value) -> PluginResult<Vec<Value>> {
    let Some(pages) = value.as_array() else {
        return Ok(Vec::new());
    };
    if pages.len() > 1000 {
        return Err(PluginError::transport(
            "Claude Code returned too many source pages.",
        ));
    }
    let mut found = Vec::new();
    for page in pages {
        if page
            .get("type")
            .is_some_and(|kind| kind != "web_search_result")
        {
            continue;
        }
        let Some(address) = cli::text(page.get("url"), 8192) else {
            continue;
        };
        let Ok(url) = url::Url::parse(address) else {
            continue;
        };
        if !["http", "https"].contains(&url.scheme())
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            continue;
        }
        if url.query_pairs().any(|(key, _)| {
            [
                "access_token",
                "refresh_token",
                "api_key",
                "apikey",
                "token",
                "secret",
                "password",
                "authorization",
                "x-amz-signature",
                "x-goog-signature",
            ]
            .contains(&key.to_ascii_lowercase().as_str())
        }) {
            continue;
        }
        let title = page
            .get("title")
            .and_then(Value::as_str)
            .filter(|title| title.len() <= 16384)
            .unwrap_or(address);
        let age = page
            .get("page_age")
            .and_then(Value::as_str)
            .filter(|age| age.len() <= 1024);
        found.push(json!({"type":"web_search_result","title":title,"url":address,"encrypted_content":"","page_age":age}));
    }
    Ok(found)
}

#[cfg(test)]
#[path = "../test/search_results.test.rs"]
mod tests;
