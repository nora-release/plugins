use async_trait::async_trait;
use futures::StreamExt;
use nora_provider_plugin_sdk::{
    CallContext, InitializeParams, PluginError, PluginResult, ProviderPlugin,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::Mutex,
    time::{Duration, Instant, SystemTime},
};

const HITS: usize = 6;
const PAGE_BYTES: usize = 1500;
const RESPONSE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Vendor {
    Tavily,
    Brave,
    Exa,
    Firecrawl,
    You,
    DeepSeek,
    SearXNG,
}
impl Vendor {
    pub fn base(self) -> &'static str {
        match self {
            Self::Tavily => "https://api.tavily.com",
            Self::Brave => "https://api.search.brave.com",
            Self::Exa => "https://api.exa.ai",
            Self::Firecrawl => "https://api.firecrawl.dev",
            Self::You => "https://ydc-index.io",
            Self::DeepSeek => "https://api.deepseek.com",
            Self::SearXNG => "",
        }
    }
    fn optional_key(self) -> bool {
        matches!(self, Self::You | Self::SearXNG)
    }
}
#[derive(Clone, Default)]
struct Settings {
    base: String,
    custom_base: bool,
    ordered: bool,
    auth_methods: Vec<Value>,
}
#[derive(Default)]
struct Pool {
    next: usize,
    rests: BTreeMap<String, Instant>,
}
impl Pool {
    fn order(&mut self, keys: &[String], ordered: bool, now: Instant) -> Vec<usize> {
        self.rests.retain(|_, until| *until > now);
        let start = if ordered {
            0
        } else {
            let index = self.next % keys.len();
            self.next = self.next.wrapping_add(1);
            index
        };
        let mut order = (0..keys.len())
            .map(|i| (start + i) % keys.len())
            .collect::<Vec<_>>();
        order.sort_by_key(|i| self.rests.get(&key_id(&keys[*i])).copied());
        order
    }
    fn rest(&mut self, key: &str, duration: Duration, now: Instant) {
        if key.is_empty() {
            return;
        }
        let until = now + duration;
        let id = key_id(key);
        self.rests
            .entry(id)
            .and_modify(|old| *old = (*old).max(until))
            .or_insert(until);
    }
}
fn key_id(key: &str) -> String {
    format!("{:x}", Sha256::digest(key.as_bytes()))
}

pub struct SearchApiPlugin {
    vendor: Vendor,
    settings: Mutex<Settings>,
    pool: Mutex<Pool>,
}
impl SearchApiPlugin {
    pub fn new(vendor: Vendor) -> Self {
        Self {
            vendor,
            settings: Mutex::new(Settings::default()),
            pool: Mutex::new(Pool::default()),
        }
    }
}
#[async_trait]
impl ProviderPlugin for SearchApiPlugin {
    fn supports_web_search(&self) -> bool {
        true
    }
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        if params.api_version != 1 {
            return Err(PluginError::invalid("Unsupported provider API version."));
        }
        let auth_methods = params
            .installed_manifest
            .as_ref()
            .unwrap_or(&params.manifest)
            .get("authMethods")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if auth_methods.len() != 1
            || auth_methods[0].get("id").and_then(Value::as_str) != Some("apiKey")
            || auth_methods[0].get("type").and_then(Value::as_str) != Some("api")
        {
            return Err(PluginError::invalid(
                "This search package must declare its API key sign-in method.",
            ));
        }
        let custom = params
            .runtime_config
            .get("baseURL")
            .or_else(|| params.runtime_config.get("baseUrl"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        *self
            .settings
            .lock()
            .map_err(|_| PluginError::invalid("Search settings are unavailable."))? = Settings {
            base: custom
                .unwrap_or(self.vendor.base())
                .trim_end_matches('/')
                .into(),
            custom_base: custom.is_some(),
            auth_methods,
            ordered: params
                .runtime_config
                .get("searchKeyPick")
                .and_then(Value::as_str)
                == Some("order")
                || params.runtime_config.get("searchKeysInOrder") == Some(&Value::Bool(true))
                || params
                    .runtime_config
                    .pointer("/pluginOptions/searchKeysInOrder")
                    == Some(&Value::Bool(true))
                || params.manifest.pointer("/config/searchKeysInOrder") == Some(&Value::Bool(true)),
        };
        Ok(json!({"apiVersion":1,"capabilities":{"sdkFeatures":{"connectionOverrides":1}}}))
    }
    async fn auth_methods(&self, _context: CallContext) -> PluginResult<Value> {
        let settings = self
            .settings
            .lock()
            .map_err(|_| PluginError::invalid("Search settings are unavailable."))?;
        Ok(json!(settings.auth_methods))
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        let method = match params.get("method").filter(|method| !method.is_null()) {
            Some(value) => value
                .as_str()
                .ok_or_else(|| PluginError::unsupported("authorize method"))?,
            None => "apiKey",
        };
        if !self
            .settings
            .lock()
            .map_err(|_| PluginError::invalid("Search settings are unavailable."))?
            .auth_methods
            .iter()
            .any(|declared| declared.get("id").and_then(Value::as_str) == Some(method))
        {
            return Err(PluginError::unsupported("authorize method"));
        }
        let inputs = params
            .get("inputs")
            .or_else(|| params.get("fields"))
            .unwrap_or(&params);
        let key = inputs
            .get("apiKey")
            .or_else(|| inputs.get("key"))
            .map(|key| {
                key.as_str()
                    .ok_or_else(|| PluginError::invalid("Enter a valid search API key."))
            })
            .transpose()?
            .unwrap_or("");
        if key.len() > 65536
            || key.contains('\0')
            || (!self.vendor.optional_key() && keys(key).is_empty())
        {
            return Err(PluginError::invalid("Enter a valid search API key."));
        }
        Ok(
            json!({"ok":true,"credentials":{"type":"apiKey","authMethod":"apiKey","key":key.trim()}}),
        )
    }
    async fn web_search(&self, context: CallContext, query: String) -> PluginResult<Value> {
        let settings = self
            .settings
            .lock()
            .map_err(|_| PluginError::invalid("Search settings are unavailable."))?
            .clone();
        let credentials = context.credentials.as_ref();
        if credentials
            .and_then(|value| value.get("type"))
            .and_then(Value::as_str)
            .is_some_and(|kind| !matches!(kind, "api" | "apiKey"))
        {
            return Err(PluginError::authentication("Use a search API key account."));
        }
        let raw = credentials
            .and_then(|value| {
                value
                    .get("key")
                    .or_else(|| value.get("access"))
                    .or_else(|| value.get("accessToken"))
            })
            .and_then(Value::as_str)
            .unwrap_or("");
        let mut keys = keys(raw);
        if keys.is_empty() {
            if !self.vendor.optional_key() {
                return Err(PluginError::authentication("Add a search API key."));
            }
            keys.push(String::new());
        }
        let order = self
            .pool
            .lock()
            .map_err(|_| PluginError::invalid("Search accounts are unavailable."))?
            .order(&keys, settings.ordered, Instant::now());
        let mut refused_for = None;
        for index in order {
            let key = &keys[index];
            match self.ask(&settings, &context, query.as_str(), key).await? {
                Answer::Pages(mut pages) => {
                    pages.retain(|page| {
                        !keys
                            .iter()
                            .any(|key| !key.is_empty() && page.url.contains(key))
                    });
                    for page in &mut pages {
                        for key in keys.iter().filter(|key| !key.is_empty()) {
                            page.title = page.title.replace(key, "[redacted]");
                            page.text = page.text.replace(key, "[redacted]");
                        }
                    }
                    self.pool
                        .lock()
                        .map_err(|_| PluginError::invalid("Search accounts are unavailable."))?
                        .rests
                        .remove(&key_id(key));
                    return Ok(pages_result(pages));
                }
                Answer::Refused(duration) => {
                    self.pool
                        .lock()
                        .map_err(|_| PluginError::invalid("Search accounts are unavailable."))?
                        .rest(key, duration, Instant::now());
                    refused_for =
                        Some(refused_for.map_or(duration, |old: Duration| old.min(duration)));
                }
            }
        }
        Ok(
            json!({"error":{"kind":"key_refused","retryAfterMs":refused_for.unwrap_or(Duration::from_secs(60)).as_millis() as u64}}),
        )
    }
}
fn keys(raw: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for key in raw
        .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .filter(|key| !key.is_empty())
    {
        if !keys.iter().any(|old| old == key) {
            keys.push(key.into());
        }
    }
    keys
}
enum Answer {
    Pages(Vec<Page>),
    Refused(Duration),
}
#[derive(Debug, PartialEq)]
struct Page {
    title: String,
    url: String,
    text: String,
}
impl SearchApiPlugin {
    async fn ask(
        &self,
        settings: &Settings,
        context: &CallContext,
        query: &str,
        key: &str,
    ) -> PluginResult<Answer> {
        let base = if self.vendor == Vendor::You && key.is_empty() && !settings.custom_base {
            "https://api.you.com"
        } else {
            &settings.base
        };
        validate_base(base)?;
        let client = nora_provider_plugin_sdk::http_client_builder(context.network_proxy.as_ref())?
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| {
                PluginError::transport("The search HTTP client could not be configured.")
            })?;
        let (method, path, body, query_params) = request(self.vendor, query, key.is_empty());
        let mut url = reqwest::Url::parse(&format!("{base}{path}"))
            .map_err(|_| PluginError::invalid("Set a valid search API address."))?;
        if !query_params.is_empty() {
            url.query_pairs_mut().extend_pairs(query_params);
        }
        let origin = url.clone();
        let mut method = method;
        let mut body = body;
        let mut redirects = 0;
        let response = loop {
            let mut request = client
                .request(method.clone(), url.clone())
                .header("accept", "application/json")
                .header("user-agent", "Nora Web Search");
            if let Some(body) = &body {
                request = request.json(body);
            }
            if !key.is_empty() {
                request = match self.vendor {
                    Vendor::Brave => request.header("X-Subscription-Token", key),
                    Vendor::Exa | Vendor::You => request.header("X-API-Key", key),
                    _ => request.bearer_auth(key),
                };
            }
            nora_provider_plugin_sdk::before_http_request().await?;
            let response = request
                .send()
                .await
                .map_err(|_| PluginError::transport("The search API could not be reached."))?;
            if !matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
                break response;
            }
            let Some(location) = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
            else {
                break response;
            };
            let next = url.join(location).map_err(|_| {
                PluginError::transport("The search API returned an invalid redirect.")
            })?;
            if next.scheme() != origin.scheme()
                || next.host_str() != origin.host_str()
                || next.port_or_known_default() != origin.port_or_known_default()
                || !next.username().is_empty()
                || next.password().is_some()
                || redirects >= 9
            {
                return Err(PluginError::transport("The search API redirected outside its configured address or too many times. Set its final base address."));
            }
            if matches!(response.status().as_u16(), 301 | 302 | 303)
                && method != reqwest::Method::GET
                && method != reqwest::Method::HEAD
            {
                method = reqwest::Method::GET;
                body = None;
            }
            redirects += 1;
            url = next;
        };
        let status = response.status().as_u16();
        if matches!(status, 401 | 402 | 403 | 429 | 432 | 433) {
            return Ok(Answer::Refused(rest_for(
                self.vendor,
                status,
                response
                    .headers()
                    .get("retry-after")
                    .and_then(|value| value.to_str().ok()),
                SystemTime::now(),
            )));
        }
        if !(200..300).contains(&status) {
            return Err(PluginError::transport(format!(
                "The search API returned HTTP {status}."
            )));
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk
                .map_err(|_| PluginError::transport("The search response was interrupted."))?;
            if bytes.len().saturating_add(chunk.len()) > RESPONSE_BYTES {
                return Err(PluginError::transport(
                    "The search response exceeds its size limit.",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| PluginError::transport("The search API returned invalid JSON."))?;
        Ok(Answer::Pages(parse(self.vendor, &value)?))
    }
}
fn validate_base(base: &str) -> PluginResult<()> {
    let url = reqwest::Url::parse(base)
        .map_err(|_| PluginError::invalid("Set a valid search API address."))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(PluginError::invalid(
            "Set an HTTP or HTTPS search API base address without credentials, query, or fragment.",
        ));
    }
    Ok(())
}
fn request(
    vendor: Vendor,
    query: &str,
    keyless: bool,
) -> (
    reqwest::Method,
    &'static str,
    Option<Value>,
    Vec<(&'static str, String)>,
) {
    use reqwest::Method;
    match vendor {
        Vendor::Tavily => (
            Method::POST,
            "/search",
            Some(json!({"query":query,"max_results":HITS,"search_depth":"basic"})),
            vec![],
        ),
        Vendor::Brave => (
            Method::GET,
            "/res/v1/web/search",
            None,
            vec![("q", query.into()), ("count", HITS.to_string())],
        ),
        Vendor::Exa => (
            Method::POST,
            "/search",
            Some(
                json!({"query":query,"numResults":HITS,"contents":{"text":{"maxCharacters":PAGE_BYTES}}}),
            ),
            vec![],
        ),
        Vendor::Firecrawl => (
            Method::POST,
            "/v2/search",
            Some(json!({"query":query,"limit":HITS})),
            vec![],
        ),
        Vendor::DeepSeek => (
            Method::POST,
            "/v1/tools/search",
            Some(json!({"query":query.chars().take(128).collect::<String>(),"count":HITS})),
            vec![],
        ),
        Vendor::SearXNG => (
            Method::GET,
            "/search",
            None,
            vec![("q", query.into()), ("format", "json".into())],
        ),
        Vendor::You if keyless => (
            Method::GET,
            "/v1/agents/search",
            None,
            vec![("query", query.into()), ("count", HITS.to_string())],
        ),
        Vendor::You => (
            Method::POST,
            "/v1/search",
            Some(json!({"query":query,"count":HITS})),
            vec![],
        ),
    }
}
fn rest_for(vendor: Vendor, status: u16, retry_after: Option<&str>, now: SystemTime) -> Duration {
    match status {
        401 | 403 => Duration::from_secs(600),
        402 => Duration::from_secs(3600),
        432 | 433 if vendor == Vendor::Tavily => Duration::from_secs(3600),
        _ => {
            let duration = retry_after
                .and_then(|value| {
                    value
                        .parse::<u64>()
                        .ok()
                        .map(Duration::from_secs)
                        .or_else(|| {
                            httpdate::parse_http_date(value)
                                .ok()
                                .map(|date| date.duration_since(now).unwrap_or_default())
                        })
                })
                .unwrap_or(Duration::from_secs(60));
            duration.clamp(Duration::from_secs(30), Duration::from_secs(600))
        }
    }
}
fn text(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or("").into()
}
fn plain(raw: &str) -> String {
    let mut without = String::new();
    let mut tag = false;
    for c in raw.chars() {
        match c {
            '<' => tag = true,
            '>' => tag = false,
            _ if !tag => without.push(c),
            _ => {}
        }
    }
    html_escape::decode_html_entities(&without)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
fn clip(raw: String) -> String {
    if raw.len() <= PAGE_BYTES {
        return raw;
    }
    let mut at = PAGE_BYTES;
    while !raw.is_char_boundary(at) {
        at -= 1;
    }
    format!("{}…", &raw[..at])
}
fn parse(vendor: Vendor, value: &Value) -> PluginResult<Vec<Page>> {
    let results = match vendor {
        Vendor::You => {
            let object = value
                .get("results")
                .and_then(Value::as_object)
                .ok_or_else(|| PluginError::transport("The search reply contains no results."))?;
            let mut values = object
                .get("web")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            values.extend(
                object
                    .get("news")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            );
            values
        }
        Vendor::Brave => value
            .pointer("/web/results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        Vendor::Firecrawl => value
            .pointer("/data/web")
            .or_else(|| value.get("data"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        _ => value
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    };
    let mut pages = Vec::new();
    for hit in results {
        let url = text(&hit, "url");
        if url.len() > 16384
            || url.chars().any(char::is_control)
            || !reqwest::Url::parse(&url).ok().is_some_and(|url| {
                matches!(url.scheme(), "http" | "https")
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
            })
        {
            continue;
        }
        let mut description = if vendor == Vendor::You {
            hit.get("snippets")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default()
        } else {
            String::new()
        };
        if description.is_empty() {
            description = [
                "content",
                "summary",
                "text",
                "description",
                "markdown",
                "snippet",
            ]
            .iter()
            .map(|key| text(&hit, key))
            .find(|text| !text.is_empty())
            .unwrap_or_default();
        }
        let title = plain(&text(&hit, "title"));
        pages.push(Page {
            title: if title.is_empty() { url.clone() } else { title },
            url,
            text: clip(plain(&description)),
        });
        if pages.len() == HITS {
            break;
        }
    }
    Ok(pages)
}
fn pages_result(pages: Vec<Page>) -> Value {
    let said = pages
        .iter()
        .enumerate()
        .map(|(index, page)| {
            format!(
                "{}. {} — {}\n{}",
                index + 1,
                page.title,
                page.url,
                page.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    json!({"text":said.trim(),"sources":pages.into_iter().map(|page|json!({"url":page.url,"title":page.title})).collect::<Vec<_>>()})
}

/// Subscription search endpoints return the same public page contract as an
/// API search, without exposing their tokens or opaque upstream fields.
pub fn provider_results(results: &[Value]) -> PluginResult<Value> {
    let mut pages = parse(Vendor::Tavily, &json!({"results":results}))?;
    pages.truncate(5);
    if pages.is_empty() {
        return Err(PluginError::transport(
            "The provider found no web search results.",
        ));
    }
    Ok(pages_result(pages))
}

#[cfg(test)]
#[path = "../test/search_api.test.rs"]
mod tests;
