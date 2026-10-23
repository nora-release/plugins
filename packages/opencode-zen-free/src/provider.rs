use crate::{catalog, collapse, protocol::Protocol, session::Sessions, sse::Frame, tools};
use async_trait::async_trait;
use futures::StreamExt;
use nora_ai::{HttpClient, HttpMethod, HttpRequest};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, PluginError, PluginResult,
    ProviderPlugin,
};
use nora_provider_subscription_core::{self as core, Core};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::sync::{Mutex, RwLock, Semaphore};
struct Catalog {
    known: BTreeMap<String, Value>,
    metadata: Value,
    protocols: BTreeMap<String, String>,
    attempted: u64,
}
pub(crate) struct ZenFree {
    core: Core,
    catalog: RwLock<Catalog>,
    discovery: Mutex<()>,
    sessions: Sessions,
    permits: Arc<Semaphore>,
}
impl Default for ZenFree {
    fn default() -> Self {
        Self {
            core: Core::new("opencode-zen-free"),
            catalog: RwLock::new(Catalog {
                known: catalog::presets(),
                metadata: catalog::bootstrap(),
                protocols: catalog::protocols(),
                attempted: 0,
            }),
            discovery: Mutex::new(()),
            sessions: Sessions::default(),
            permits: Arc::new(Semaphore::new(2)),
        }
    }
}
fn require(credentials: Option<&Value>) -> PluginResult<()> {
    let credentials = credentials
        .ok_or_else(|| PluginError::authentication("Enable OpenCode Zen free access first."))?;
    if credentials["type"] != "api" || credentials["key"] != "public" {
        return Err(PluginError::authentication("Zen Free uses the public credential. Add private OpenCode keys to the separate paid provider."));
    }
    Ok(())
}
fn clean_headers(headers: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter(|(key, _)| {
            ![
                "content-length",
                "content-encoding",
                "transfer-encoding",
                "connection",
                "set-cookie",
            ]
            .contains(&key.to_ascii_lowercase().as_str())
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}
fn header(request: &FetchRequest, name: &str) -> Option<String> {
    request
        .headers
        .iter()
        .find(|(key, value)| {
            key.eq_ignore_ascii_case(name)
                && !value.is_empty()
                && value.len() <= 4096
                && !value.contains(['\r', '\n', '\0'])
        })
        .map(|(_, value)| value.clone())
}
fn failure(protocol: Protocol, message: &str) -> FetchResponse {
    FetchResponse::bytes(
        502,
        BTreeMap::from([("content-type".into(), "application/json".into())]),
        serde_json::to_vec(&tools::error(protocol, message)).unwrap(),
    )
}
impl ZenFree {
    async fn read(
        &self,
        client: &dyn HttpClient,
        url: &str,
        public: bool,
    ) -> PluginResult<Vec<u8>> {
        let mut request = HttpRequest::new(HttpMethod::Get, url);
        if public {
            request = request
                .with_header("authorization", "Bearer public")
                .with_header("x-opencode-client", "cli");
        }
        let response = tokio::time::timeout(Duration::from_secs(15), client.stream(request))
            .await
            .map_err(|_| PluginError::transport("Zen model discovery timed out."))?
            .map_err(|_| PluginError::transport("Zen model discovery could not connect."))?;
        if !(200..300).contains(&response.status) {
            return Err(PluginError::transport(format!(
                "Zen model discovery returned HTTP {}.",
                response.status
            )));
        }
        tokio::time::timeout(
            Duration::from_secs(15),
            core::collect(response, 16 * 1024 * 1024),
        )
        .await
        .map_err(|_| PluginError::transport("Zen model discovery timed out."))?
    }
    async fn discover(&self, client: &dyn HttpClient) -> PluginResult<Vec<Value>> {
        let _guard = self.discovery.lock().await;
        self.catalog.write().await.attempted = core::now();
        let listing_url = format!("{}/models", catalog::BASE);
        let (listing, metadata, docs) = tokio::join!(
            self.read(client, &listing_url, true),
            self.read(client, catalog::CATALOG, false),
            self.read(client, catalog::DOCS, false)
        );
        let listing: Value = serde_json::from_slice(&listing?)
            .map_err(|_| PluginError::transport("Zen returned an invalid model listing."))?;
        let ids = listing["data"]
            .as_array()
            .ok_or_else(|| PluginError::transport("Zen returned no model listing."))?
            .iter()
            .filter_map(|value| value["id"].as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        if ids.len() > core::MAX_MODELS {
            return Err(PluginError::transport("Zen returned too many models."));
        }
        let mut current = self.catalog.write().await;
        if let Some(provider) = metadata
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|value| {
                value
                    .get("opencode")
                    .filter(|provider| provider["models"].is_object())
                    .cloned()
            })
        {
            current.metadata = provider;
        }
        if let Some(parsed) = docs
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .map(|text| catalog::parse_docs(&text))
            .filter(|protocols| !protocols.is_empty())
        {
            current.protocols = catalog::protocols();
            current.protocols.extend(parsed);
        }
        let mut known =
            catalog::build(ids.clone(), &current.metadata, &current.protocols, "remote")?;
        for id in ids {
            if !known.contains_key(&id)
                && current.metadata["models"][&id].is_null()
                && !current.protocols.contains_key(&id)
            {
                if let Some(previous) = current.known.get(&id) {
                    let mut previous = previous.clone();
                    previous["info"]["catalogSource"] = json!("remote");
                    known.insert(id, previous);
                }
            }
        }
        current.known = known;
        Ok(current.known.values().cloned().collect())
    }
    fn headers(
        &self,
        request: &FetchRequest,
        body: &Value,
        protocol: Protocol,
    ) -> BTreeMap<String, String> {
        let session = self.sessions.for_request(request, body);
        let mut headers = BTreeMap::from([
            ("content-type".into(), "application/json".into()),
            ("accept".into(), "*/*".into()),
            ("user-agent".into(), protocol.agent()),
            (
                "x-opencode-project".into(),
                header(request, "x-opencode-project").unwrap_or_else(|| "global".into()),
            ),
            ("x-opencode-session".into(), session.clone()),
            ("x-opencode-session-id".into(), session),
            ("x-opencode-request".into(), self.sessions.id("msg", false)),
            ("x-opencode-client".into(), "cli".into()),
        ]);
        if protocol == Protocol::Anthropic {
            headers.insert("x-api-key".into(), "public".into());
            headers.insert(
                "anthropic-version".into(),
                header(request, "anthropic-version").unwrap_or_else(|| "2023-06-01".into()),
            );
            if let Some(beta) = header(request, "anthropic-beta") {
                headers.insert("anthropic-beta".into(), beta);
            }
        } else {
            headers.insert("authorization".into(), "Bearer public".into());
        }
        headers
    }
}
#[async_trait]
impl ProviderPlugin for ZenFree {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        self.core.initialize(params)
    }
    async fn auth_methods(&self, _: CallContext) -> PluginResult<Value> {
        Ok(self.core.config()?.manifest["authMethods"].clone())
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        if core::required(&params, "authMethodID")? != "public" {
            return Err(PluginError::unsupported("authMethodID"));
        }
        if let Some(key) = core::inputs(&params)
            .get("key")
            .and_then(Value::as_str)
            .filter(|key| !key.is_empty())
        {
            if key != "public" {
                return Err(PluginError::invalid(
                    "Zen Free accepts only the public credential.",
                ));
            }
        }
        let client = self.core.params_client(&params)?;
        let bytes = self
            .read(client.as_ref(), &format!("{}/models", catalog::BASE), true)
            .await?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| PluginError::transport("Zen returned an invalid model listing."))?;
        if !value["data"].is_array() {
            return Err(PluginError::transport(
                "Zen did not confirm its public model listing.",
            ));
        }
        Ok(
            json!({"ok":true,"credentials":{"type":"api","authMethod":"public","provider":"opencode-zen-free","key":"public","expires":0,"accountId":"public","label":"OpenCode Zen Free"}}),
        )
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        require(params.get("credentials"))?;
        Ok(json!({"changed":false}))
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        if context.credentials.is_none() {
            return Ok(core::listing(
                catalog::presets().into_values().collect(),
                "openai-completions",
                catalog::BASE,
                "preset",
            ));
        }
        require(context.credentials.as_ref())?;
        let client = self.core.client(context.network_proxy.as_ref())?;
        Ok(core::listing(
            self.discover(client.as_ref()).await?,
            "openai-completions",
            catalog::BASE,
            "remote",
        ))
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        require(context.credentials.as_ref())?;
        Ok(
            json!({"available":false,"account":{"id":context.account,"plan":"OpenCode Zen Free"},"plan":"OpenCode Zen Free","items":[],"reason":"OpenCode does not report an account quota for its public free models."}),
        )
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        require(request.credentials.as_ref())?;
        let url = core::validate_fetch(
            &request,
            catalog::BASE,
            &[
                "/inference/v1/chat/completions",
                "/inference/v1/responses",
                "/inference/v1/messages",
                "/zen/v1/chat/completions",
                "/zen/v1/responses",
                "/zen/v1/messages",
            ],
        )?;
        if url.query().is_some() {
            return Err(PluginError::invalid(
                "The Zen request URL must not contain a query.",
            ));
        }
        let protocol = [Protocol::Chat, Protocol::Responses, Protocol::Anthropic]
            .into_iter()
            .find(|protocol| {
                url.path() == format!("/inference/v1{}", protocol.path())
                    || url.path() == format!("/zen/v1{}", protocol.path())
            })
            .ok_or_else(|| PluginError::invalid("Unsupported Zen endpoint."))?;
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| PluginError::transport("Zen's request limit is unavailable."))?;
        let mut body: Value = serde_json::from_slice(
            &request
                .decode_body()?
                .ok_or_else(|| PluginError::invalid("The Zen request body is missing."))?,
        )
        .map_err(|_| PluginError::invalid("The Zen request is invalid JSON."))?;
        let model = core::required(&body, "model")?.to_owned();
        let client = self.core.client(request.network_proxy.as_ref())?;
        let (missing, attempted) = {
            let catalog = self.catalog.read().await;
            (!catalog.known.contains_key(&model), catalog.attempted)
        };
        if missing && (attempted == 0 || core::now().saturating_sub(attempted) >= 60_000) {
            let _ = self.discover(client.as_ref()).await;
        }
        let selected = self.catalog.read().await.known.get(&model).cloned();
        let Some(selected) = selected else {
            return Ok(FetchResponse::bytes(404,BTreeMap::from([("content-type".into(),"application/json".into())]),serde_json::to_vec(&json!({"type":"error","error":{"type":"not_found_error","message":"This model is not available as an OpenCode Zen free model now. Refresh the model list."}})).unwrap()));
        };
        if selected["api"] != protocol.api() {
            return Err(PluginError::invalid(
                "This Zen free model requires a different protocol. Refresh the model list.",
            ));
        }
        let streaming = body["stream"] == true;
        body["stream"] = json!(true);
        let names = tools::normalize(&mut body, protocol)?;
        if protocol == Protocol::Chat {
            if body["stream_options"].is_null() {
                body["stream_options"] = json!({});
            }
            if !body["stream_options"].is_object() {
                return Err(PluginError::invalid(
                    "Zen stream_options must be an object.",
                ));
            }
            body["stream_options"]["include_usage"] = json!(true);
        }
        let outgoing = HttpRequest {
            method: HttpMethod::Post,
            url: format!("{}{}", catalog::BASE, protocol.path()),
            headers: self.headers(&request, &body, protocol),
            body: Some(
                serde_json::to_vec(&body)
                    .map_err(|_| PluginError::invalid("Invalid Zen request body."))?,
            ),
        };
        let response = tokio::time::timeout(Duration::from_secs(60), client.stream(outgoing))
            .await
            .map_err(|_| PluginError::transport("Zen did not answer in time."))?
            .map_err(|_| PluginError::transport("Zen could not be reached."))?;
        let status = response.status;
        let mut headers = clean_headers(&response.headers);
        let event_stream = headers.iter().any(|(key, value)| {
            key.eq_ignore_ascii_case("content-type")
                && value
                    .split(';')
                    .next()
                    .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
        });
        if !(200..300).contains(&status) {
            let body =
                futures::stream::unfold((response.body, permit), |(mut body, permit)| async move {
                    match tokio::time::timeout(Duration::from_secs(180), body.next()).await {
                        Ok(Some(Ok(bytes))) => Some((Ok(bytes.to_vec()), (body, permit))),
                        Ok(None) => None,
                        _ => Some((
                            Err(PluginError::transport("Zen's error response stopped.")),
                            (
                                Box::pin(futures::stream::empty()) as nora_ai::HttpByteStream,
                                permit,
                            ),
                        )),
                    }
                });
            return Ok(FetchResponse {
                status,
                headers,
                body: Box::pin(body),
            });
        }
        if !event_stream {
            let bytes = core::collect(response, 32 * 1024 * 1024).await?;
            let mut value: Value = match serde_json::from_slice(&bytes) {
                Ok(value) => value,
                Err(_) => return Ok(failure(protocol, "Zen returned invalid response JSON.")),
            };
            if let Err(error) = tools::snapshot(&mut value, protocol, &names) {
                return Ok(failure(protocol, &error.message));
            }
            headers.insert("content-type".into(), "application/json".into());
            return Ok(FetchResponse::bytes(
                status,
                headers,
                serde_json::to_vec(&value).unwrap(),
            ));
        }
        let guard = tools::guard(response.body, protocol, names, streaming);
        if !streaming {
            return Ok(match collapse::collapse(guard, protocol).await {
                Ok(value) => {
                    headers.insert("content-type".into(), "application/json".into());
                    FetchResponse::bytes(status, headers, serde_json::to_vec(&value).unwrap())
                }
                Err(error) => failure(protocol, &error.message),
            });
        }
        let body = futures::stream::unfold(
            (guard, permit, false),
            move |(mut guard, permit, stopped)| async move {
                if stopped {
                    return None;
                }
                match guard.next().await {
                    Some(Ok(frame)) => Some((Ok(frame.encoded()), (guard, permit, false))),
                    Some(Err(error)) => {
                        let frame = Frame::json(
                            tools::error(protocol, &error.message),
                            if protocol == Protocol::Chat {
                                ""
                            } else {
                                "error"
                            },
                        );
                        Some((Ok(frame.encoded()), (guard, permit, true)))
                    }
                    None => None,
                }
            },
        );
        Ok(FetchResponse {
            status,
            headers,
            body: Box::pin(body),
        })
    }
}
#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
