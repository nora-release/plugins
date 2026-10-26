use crate::{auth, catalog, generate, readings};
use async_trait::async_trait;
use futures::StreamExt;
use nora_ai::{HttpClient, HttpMethod, HttpRequest, HttpStreamingResponse};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, PluginError, PluginResult,
    ProviderPlugin,
};
use nora_provider_subscription_core::{self as core, Core};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, time::Duration};
use tokio::sync::Mutex;
const API: &str = "https://api.commandcode.ai";
pub(crate) struct CommandCode {
    core: Core,
    browsers: Mutex<BTreeMap<String, auth::Browser>>,
    plans: Mutex<BTreeMap<String, (u64, Value)>>,
    session: String,
}
impl Default for CommandCode {
    fn default() -> Self {
        Self {
            core: Core::new("commandcode-plan"),
            browsers: Mutex::new(BTreeMap::new()),
            plans: Mutex::new(BTreeMap::new()),
            session: uuid::Uuid::new_v4().to_string(),
        }
    }
}
impl CommandCode {
    async fn account(&self, client: &dyn HttpClient, path: &str, key: &str) -> PluginResult<Value> {
        let (status, value) = core::exchange(
            client,
            core::json_request(HttpMethod::Get, format!("{API}{path}"), None, Some(key))?,
        )
        .await?;
        if status == 401 || status == 403 {
            return Err(PluginError::authentication(
                "Command Code refused this key. Check the selected account.",
            ));
        }
        if !(200..300).contains(&status) {
            return Err(PluginError::transport(format!(
                "Command Code returned HTTP {status}; the account was kept."
            )));
        }
        Ok(value)
    }
    async fn who(&self, client: &dyn HttpClient, key: &str) -> PluginResult<Value> {
        let mut last = None;
        for _ in 0..3 {
            match self.account(client, "/alpha/whoami", key).await {
                Ok(value) => {
                    let user = &value["user"];
                    if ["id", "userId", "email", "userName"]
                        .iter()
                        .any(|key| core::text(&user[key]).is_some())
                    {
                        return Ok(user.clone());
                    }
                    return Err(PluginError::transport(
                        "Command Code did not identify this account.",
                    ));
                }
                Err(error) if error.code == "authentication_required" => return Err(error),
                Err(error) => last = Some(error),
            }
        }
        Err(last.unwrap_or_else(|| {
            PluginError::transport("Command Code could not identify this account.")
        }))
    }
    async fn subscription(&self, client: &dyn HttpClient, credentials: &Value) -> Option<Value> {
        let key = core::bearer(credentials).ok()?;
        let digest = core::fingerprint(&json!(key));
        let previous = self.plans.lock().await.get(&digest).cloned();
        if let Some((at, value)) = &previous {
            if core::now().saturating_sub(*at) < 600_000 {
                return Some(value.clone());
            }
        }
        let read = tokio::time::timeout(
            Duration::from_secs(15),
            self.account(client, "/alpha/billing/subscriptions", key),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .and_then(|value| readings::subscription(&value));
        if let Some(value) = read {
            let mut plans = self.plans.lock().await;
            if plans.len() >= 512 {
                plans.retain(|_, (at, _)| core::now().saturating_sub(*at) < 600_000);
                if plans.len() >= 512 {
                    plans.clear();
                }
            }
            plans.insert(digest, (core::now(), value.clone()));
            return Some(value);
        }
        previous.map(|(_, value)| value).or_else(|| {
            credentials
                .get("plan")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .map(|name| json!({"id":credentials["planId"].as_str().unwrap_or(""),"plan":name}))
        })
    }
    async fn signed_in(
        &self,
        client: &dyn HttpClient,
        mut credentials: Value,
    ) -> PluginResult<Value> {
        let key = core::bearer(&credentials)?.to_owned();
        let user = self.who(client, &key).await?;
        let id = ["id", "userId", "email", "userName"]
            .iter()
            .find_map(|key| core::text(&user[key]))
            .unwrap();
        credentials["uid"] = json!(id);
        credentials["accountId"] = json!(id);
        credentials["label"] = json!(core::text(&user["userName"])
            .or_else(|| core::text(&user["email"]))
            .unwrap_or(id));
        credentials["email"] = user["email"].clone();
        if let Some(sub) = self.subscription(client, &credentials).await {
            credentials["plan"] = sub["plan"].clone();
            credentials["planId"] = sub["id"].clone();
        }
        Ok(credentials)
    }
    async fn live(&self, stored: &Value, client: &dyn HttpClient) -> PluginResult<Value> {
        let current = self.core.latest(stored).await?;
        if current["provider"] != "commandcode-plan" {
            return Err(PluginError::authentication(
                "Reconnect this Command Code account.",
            ));
        }
        if current["authMethod"] != "cli" {
            core::bearer(&current)?;
            return Ok(current);
        }
        let path = core::required(&current["source"], "path")?.to_owned();
        let (_, source) = tokio::task::spawn_blocking(move || auth::read_source(&path))
            .await
            .map_err(|_| {
                PluginError::authentication(
                    "The selected Command Code CLI account could not be read.",
                )
            })??;
        let key = core::bearer(&source)?;
        if current["source"]["keyDigest"] != core::fingerprint(&json!(key)) {
            let user = self.who(client, key).await?;
            let uid = ["id", "userId", "email", "userName"]
                .iter()
                .find_map(|key| core::text(&user[key]))
                .unwrap_or("");
            if core::text(&current["uid"]) != Some(uid) {
                return Err(PluginError::authentication("The selected Command Code file now belongs to another account. Reconnect it explicitly."));
            }
        }
        let mut live = current;
        live["key"] = json!(key);
        Ok(live)
    }
    async fn mark_go(&self, stored: &Value, live: &Value) -> PluginResult<()> {
        let key = core::bearer(live)?;
        let mut plans = self.plans.lock().await;
        if plans.len() >= 512 {
            plans.clear();
        }
        plans.insert(
            core::fingerprint(&json!(key)),
            (core::now(), json!({"plan":"Go","id":""})),
        );
        drop(plans);
        let mut next = self.core.latest(stored).await?;
        if next["plan"] != "Go" {
            next["plan"] = json!("Go");
            next["planId"] = json!("");
            self.core.persist_rotation(stored, next).await?;
        }
        Ok(())
    }
    fn headers(
        &self,
        credentials: &Value,
        request: &FetchRequest,
        go: bool,
    ) -> PluginResult<BTreeMap<String, String>> {
        let key = core::bearer(credentials)?;
        let mut headers = BTreeMap::from([
            ("authorization".into(), format!("Bearer {key}")),
            ("content-type".into(), "application/json".into()),
            ("accept".into(), "*/*".into()),
        ]);
        if go {
            let session = request
                .metadata
                .get("sessionID")
                .map(|session| {
                    let bytes =
                        Sha256::digest(format!("commandcode session\0{session}").as_bytes());
                    uuid::Uuid::from_slice(&bytes[..16]).unwrap().to_string()
                })
                .unwrap_or_else(|| self.session.clone());
            headers.extend(BTreeMap::from([
                ("user-agent".into(), "cli".into()),
                ("x-command-code-version".into(), "1.72.2".into()),
                ("x-cli-environment".into(), "production".into()),
                ("x-project-slug".into(), "nora".into()),
                ("x-taste-learning".into(), "false".into()),
                ("x-session-id".into(), session),
            ]));
        } else {
            headers.insert("x-api-key".into(), key.to_owned());
            for (key, value) in &request.headers {
                if ["anthropic-version", "anthropic-beta", "openai-beta"]
                    .contains(&key.to_ascii_lowercase().as_str())
                {
                    headers.insert(key.to_ascii_lowercase(), value.clone());
                }
            }
        }
        if self.core.config()?.manifest["config"]["zdr"] == true {
            headers.insert("x-cmd-zdr".into(), "1".into());
        }
        Ok(headers)
    }
    async fn generate(
        &self,
        client: &dyn HttpClient,
        request: &FetchRequest,
        credentials: &Value,
        chat: &Value,
    ) -> PluginResult<FetchResponse> {
        let dir = request.metadata.get("workingDirectory").map(String::as_str);
        let git = request
            .metadata
            .get("isGitRepository")
            .is_some_and(|value| value == "true");
        let body = generate::request(chat, dir, git)?;
        let outgoing = HttpRequest {
            method: HttpMethod::Post,
            url: format!("{API}/alpha/generate"),
            headers: self.headers(credentials, request, true)?,
            body: Some(
                serde_json::to_vec(&body)
                    .map_err(|_| PluginError::invalid("Invalid Go request body."))?,
            ),
        };
        let response = tokio::time::timeout(Duration::from_secs(60), client.stream(outgoing))
            .await
            .map_err(|_| PluginError::transport("Command Code did not answer in time."))?
            .map_err(|_| PluginError::transport("Command Code could not be reached."))?;
        generate::response(response, chat).await
    }
}
#[async_trait]
impl ProviderPlugin for CommandCode {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        self.core.initialize(params)
    }
    async fn auth_methods(&self, _: CallContext) -> PluginResult<Value> {
        Ok(self.core.config()?.manifest["authMethods"].clone())
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        let method = core::required(&params, "authMethodID")?;
        let client = self.core.params_client(&params)?;
        match method {
            "apiKey" => {
                let key = core::required(core::inputs(&params), "key")?;
                let credentials = self
                    .signed_in(
                        client.as_ref(),
                        auth::credential(&json!({"apiKey":key}), "apiKey")?,
                    )
                    .await?;
                Ok(json!({"ok":true,"credentials":credentials}))
            }
            "cli" => {
                let path = match core::inputs(&params).get("sourcePath") {
                    None | Some(Value::Null) => String::new(),
                    Some(Value::String(path)) => path.clone(),
                    Some(_) => {
                        return Err(PluginError::invalid("Select a Command Code account file."))
                    }
                };
                let (path, source) = tokio::task::spawn_blocking(move || auth::read_source(&path))
                    .await
                    .map_err(|_| {
                        PluginError::authentication("The selected account file could not be read.")
                    })??;
                let mut credentials = self.signed_in(client.as_ref(), source).await?;
                let digest = core::fingerprint(&json!(core::bearer(&credentials)?));
                credentials.as_object_mut().unwrap().remove("key");
                credentials["source"] = json!({"kind":"cli","path":path,"keyDigest":digest});
                Ok(json!({"ok":true,"credentials":credentials}))
            }
            "oauth" => {
                let (url, browser) = auth::browser().await?;
                let mut browsers = self.browsers.lock().await;
                browsers.retain(|_, pending| pending.until > core::now());
                if browsers.len() >= 16 {
                    return Err(PluginError::invalid(
                        "Finish an earlier Command Code sign-in first.",
                    ));
                }
                let session = self
                    .core
                    .begin(json!({"method":"browser"}), browser.until)
                    .await?;
                browsers.insert(session.clone(), browser);
                Ok(
                    json!({"session":session,"url":url,"method":"auto","instructions":"Approve the key for this machine in Command Code Studio."}),
                )
            }
            _ => Err(PluginError::unsupported(method)),
        }
    }
    async fn callback(&self, params: Value) -> PluginResult<Value> {
        let session = core::required(&params, "session")?;
        self.core.take_pending(&params).await?;
        let mut browser = self
            .browsers
            .lock()
            .await
            .remove(session)
            .ok_or_else(|| PluginError::authentication("The Command Code sign-in expired."))?;
        let credentials = (&mut browser.receive).await.map_err(|_| {
            PluginError::authentication("The Command Code sign-in was cancelled.")
        })??;
        let client = self.core.params_client(&params)?;
        let credentials = self.signed_in(client.as_ref(), credentials).await?;
        Ok(json!({"ok":true,"credentials":credentials}))
    }
    async fn refresh(&self, params: Value) -> PluginResult<Value> {
        let client = self.core.params_client(&params)?;
        self.live(
            params
                .get("credentials")
                .ok_or_else(|| PluginError::authentication("Connect this Command Code account."))?,
            client.as_ref(),
        )
        .await?;
        Ok(json!({"changed":false}))
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        let Some(stored) = context.credentials else {
            return Ok(core::listing(
                catalog::presets("", false),
                "openai-completions",
                catalog::BASE,
                "preset",
            ));
        };
        let client = self.core.client(context.network_proxy.as_ref())?;
        let live = self.live(&stored, client.as_ref()).await?;
        let (sub, credits) = tokio::join!(
            self.subscription(client.as_ref(), &live),
            self.account(
                client.as_ref(),
                "/alpha/billing/credits",
                core::bearer(&live)?
            )
        );
        let plan = sub
            .as_ref()
            .and_then(|sub| core::text(&sub["plan"]))
            .unwrap_or("");
        let extra = credits.as_ref().map(readings::extra).unwrap_or(false);
        let mut request = core::json_request(
            HttpMethod::Get,
            format!("{}/models", catalog::BASE),
            None,
            if plan == "Go" {
                None
            } else {
                Some(core::bearer(&live)?)
            },
        )?;
        if plan != "Go" {
            request
                .headers
                .insert("x-api-key".into(), core::bearer(&live)?.into());
        }
        let models = core::checked(client.as_ref(), request)
            .await
            .and_then(|value| catalog::remote(&value, plan, extra));
        Ok(match models {
            Ok(models) => core::listing(models, "openai-completions", catalog::BASE, "remote"),
            Err(_) => core::listing(
                catalog::presets(plan, extra),
                "openai-completions",
                catalog::BASE,
                "preset",
            ),
        })
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let client = self.core.client(context.network_proxy.as_ref())?;
        let stored = context
            .credentials
            .as_ref()
            .ok_or_else(|| PluginError::authentication("Connect Command Code to read usage."))?;
        let live = self.live(stored, client.as_ref()).await?;
        let (sub, credits) = tokio::join!(
            self.subscription(client.as_ref(), &live),
            self.account(
                client.as_ref(),
                "/alpha/billing/credits",
                core::bearer(&live)?
            )
        );
        if let Some(sub) = &sub {
            let mut next = self.core.latest(stored).await?;
            if next["plan"] != sub["plan"] || next["planId"] != sub["id"] {
                next["plan"] = sub["plan"].clone();
                next["planId"] = sub["id"].clone();
                self.core.persist_rotation(stored, next).await?;
            }
        }
        Ok(readings::usage(
            credits.as_ref().ok(),
            sub.as_ref(),
            &context.account,
        ))
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        core::validate_fetch(
            &request,
            catalog::BASE,
            &[
                "/provider/v1/chat/completions",
                "/provider/v1/responses",
                "/provider/v1/messages",
            ],
        )?;
        let client = self.core.client(request.network_proxy.as_ref())?;
        let stored = request
            .credentials
            .as_ref()
            .ok_or_else(|| PluginError::authentication("Connect a Command Code account."))?;
        let live = self.live(stored, client.as_ref()).await?;
        let bytes = request
            .decode_body()?
            .ok_or_else(|| PluginError::invalid("The Command Code request body is missing."))?;
        let chat =
            if request.url.ends_with("/chat/completions") {
                Some(serde_json::from_slice::<Value>(&bytes).map_err(|_| {
                    PluginError::invalid("The Command Code request is invalid JSON.")
                })?)
            } else {
                None
            };
        if let Some(chat) = &chat {
            if self
                .subscription(client.as_ref(), &live)
                .await
                .as_ref()
                .is_some_and(|sub| sub["plan"] == "Go")
            {
                return self.generate(client.as_ref(), &request, &live, chat).await;
            }
        }
        let outgoing = HttpRequest {
            method: HttpMethod::Post,
            url: request.url.clone(),
            headers: self.headers(&live, &request, false)?,
            body: Some(bytes),
        };
        let response = tokio::time::timeout(Duration::from_secs(60), client.stream(outgoing))
            .await
            .map_err(|_| PluginError::transport("Command Code did not answer in time."))?
            .map_err(|_| PluginError::transport("Command Code could not be reached."))?;
        let status = response.status;
        let mut headers = response.headers.clone();
        headers.retain(|key, _| {
            ![
                "content-length",
                "content-encoding",
                "transfer-encoding",
                "connection",
                "set-cookie",
            ]
            .contains(&key.to_ascii_lowercase().as_str())
        });
        if !(200..300).contains(&status) {
            let bytes = core::collect(response, 1024 * 1024).await?;
            let message = String::from_utf8_lossy(&bytes).to_lowercase();
            if message.contains("go plan doesn") && message.contains("include api access") {
                self.mark_go(stored, &live).await?;
                if let Some(chat) = chat {
                    return self.generate(client.as_ref(), &request, &live, &chat).await;
                }
            }
            return Ok(FetchResponse::bytes(status, headers, bytes));
        }
        let body = futures::stream::unfold(response.body, |mut body| async move {
            match tokio::time::timeout(Duration::from_secs(180), body.next()).await {
                Ok(Some(Ok(bytes))) => Some((Ok(bytes.to_vec()), body)),
                Ok(None) => None,
                _ => Some((
                    Err(PluginError::transport("Command Code's response stopped.")),
                    Box::pin(futures::stream::empty()) as nora_ai::HttpByteStream,
                )),
            }
        });
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
