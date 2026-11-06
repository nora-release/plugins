use async_trait::async_trait;
use nora_ai::{HttpClient, HttpMethod};
use nora_provider_plugin_sdk::{
    CallContext, FetchRequest, FetchResponse, InitializeParams, PluginError, PluginResult,
    ProviderPlugin,
};
use nora_provider_subscription_core::{self as core, Core};
use serde_json::{json, Value};
use std::time::Duration;

const CONTROL: &str = "https://amr-api.open-design.ai";
const BASE: &str = "https://amr-link.open-design.ai/v1";
const SOURCE: &str = "opencode-opendesign-auth/0.1.6";
pub struct OpenDesign {
    core: Core,
}
impl Default for OpenDesign {
    fn default() -> Self {
        Self {
            core: Core::new("opendesign"),
        }
    }
}
fn runtime_key(credentials: &Value) -> PluginResult<&str> {
    let key = core::bearer(credentials)?;
    if key.starts_with("vela_ctrl_") {
        return Err(PluginError::authentication("An Open Design control key cannot send model requests. Sign in with the account or use its runtime key."));
    }
    Ok(key)
}
fn control_key(credentials: &Value) -> PluginResult<&str> {
    core::text(&credentials["control"])
        .or_else(|| core::text(&credentials["metadata"]["control"]))
        .or_else(|| core::text(&credentials["key"]).filter(|key| key.starts_with("vela_ctrl_")))
        .ok_or_else(|| PluginError::unsupported("account allowance with a runtime API key"))
}
fn auth_of(value: &Value) -> PluginResult<Value> {
    let user = &value["user"];
    let runtime = core::text(&value["runtimeKey"])
        .or_else(|| core::text(&user["runtimeKey"]))
        .ok_or_else(|| PluginError::authentication("Open Design did not return a runtime key."))?;
    let control = core::text(&value["controlKey"])
        .or_else(|| core::text(&user["controlKey"]))
        .ok_or_else(|| PluginError::authentication("Open Design did not return a control key."))?;
    if !runtime.starts_with("sk-amr-") || !control.starts_with("vela_ctrl_") {
        return Err(PluginError::authentication(
            "Open Design returned invalid account keys.",
        ));
    }
    let email = core::text(&user["email"]).unwrap_or("");
    Ok(
        json!({"type":"oauth","authMethod":"oauth","access":runtime,"control":control,"refresh":"","expires":0,
        "email":email,"accountId":email,"uid":core::text(&user["id"]).or_else(|| core::text(&user["appUserId"])),
        "label":if email.is_empty(){"Open Design account"}else{email}}),
    )
}
fn api(
    method: HttpMethod,
    path: &str,
    value: Option<&Value>,
    key: Option<&str>,
) -> PluginResult<nora_ai::HttpRequest> {
    Ok(
        core::json_request(method, format!("{CONTROL}{path}"), value, key)?
            .with_header("user-agent", SOURCE),
    )
}
fn seed() -> Value {
    serde_json::from_str(include_str!("models.json")).unwrap_or_else(|_| json!({}))
}
fn api_of(id: &str) -> &'static str {
    match id {
        "claude-haiku-5.5" | "deepseek-v4-pro" => "openai-responses",
        "glm-5.3-flash" => "anthropic-messages",
        _ => "openai-completions",
    }
}
fn model(id: &str, entry: &Value, preset: &Value, source: &str) -> Value {
    let metadata = &entry["metadata"];
    let context = metadata["context_limit"]
        .as_u64()
        .or(preset["context"].as_u64())
        .unwrap_or(200_000);
    let output = metadata["context_budget"]
        .as_u64()
        .or(preset["output"].as_u64())
        .unwrap_or(32_000);
    let modalities = if entry.is_null() {
        preset["input"].as_array()
    } else {
        entry
            .pointer("/architecture/input_modalities")
            .and_then(Value::as_array)
    };
    let input: Vec<_> = ["text", "image", "audio", "video", "pdf"]
        .into_iter()
        .filter(|kind| {
            *kind == "text" || modalities.is_some_and(|list| list.iter().any(|item| item == kind))
        })
        .collect();
    let efforts: Vec<String> = preset["efforts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|value| core::text(value).map(str::to_owned))
        .collect();
    let thinking = !efforts.is_empty()
        || core::text(&metadata["capability"]).or_else(|| core::text(&preset["capability"]))
            == Some("advanced");
    let mut model = core::model(
        id,
        core::text(&preset["name"]).unwrap_or(id),
        api_of(id),
        "opendesign",
        BASE,
        context,
        output,
        input.contains(&"image"),
        thinking,
        efforts,
        source,
    );
    model["input"] = json!(input);
    let cost = if entry.is_null() {
        &preset["cost"]
    } else {
        &metadata["cost"]
    };
    let multiplier = match core::text(cost) {
        Some("low") => Some(0.5),
        Some("medium") => Some(1.0),
        Some("high") => Some(2.0),
        Some("very_high") => Some(4.0),
        _ => None,
    };
    if let Some(multiplier) = multiplier {
        model["info"]["creditMultiplier"] = json!(multiplier);
    }
    model["info"]["pricingKnown"] = json!(false);
    model
}
fn models_of(value: &Value) -> PluginResult<Vec<Value>> {
    let rows = value["data"]
        .as_array()
        .ok_or_else(|| PluginError::transport("Open Design returned an invalid model list."))?;
    if rows.len() > core::MAX_MODELS {
        return Err(PluginError::transport(
            "Open Design returned too many models.",
        ));
    }
    let defaults = seed();
    let mut seen = std::collections::BTreeSet::new();
    let mut result = Vec::new();
    for row in rows {
        let id = core::required(row, "id")?;
        if id.len() > 2048 {
            return Err(PluginError::transport(
                "Open Design returned an invalid model ID.",
            ));
        }
        if seen.insert(id.to_owned()) {
            result.push(model(id, row, &defaults[id], "remote"));
        }
    }
    Ok(result)
}
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
        .filter(|value| value.is_finite())
}
fn window_name(span: Option<f64>) -> String {
    let Some(seconds) = span else {
        return "Allowance".into();
    };
    let (amount, unit) = if seconds % 86400.0 == 0.0 {
        (seconds / 86400.0, "day")
    } else if seconds % 3600.0 == 0.0 {
        (seconds / 3600.0, "hour")
    } else {
        ((seconds / 60.0).round(), "minute")
    };
    format!("{amount} {unit}{}", if amount == 1.0 { "" } else { "s" })
}
fn usage_of(preflight: &Value, summary: &Value, account: &str) -> Value {
    let mut items = Vec::new();
    for window in preflight
        .pointer("/codingPlan/windows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(used), Some(limit)) = (
            number(&window["usedCredits"]),
            number(&window["limitCredits"]),
        ) else {
            continue;
        };
        if limit <= 0.0 {
            continue;
        }
        let span = number(&window["durationSeconds"]).filter(|value| *value > 0.0);
        let mut item = core::usage_item(
            &window_name(span),
            used,
            limit,
            core::timestamp(&window["resetsAt"]),
            span,
            true,
        );
        if let Some(percent) = item["usedPercent"].as_f64() {
            item["usedPercent"] = json!((percent * 10.0).round() / 10.0);
        }
        items.push(item);
    }
    items.sort_by(|a, b| {
        a["windowSeconds"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&b["windowSeconds"].as_f64().unwrap_or(0.0))
    });
    let tier = core::text(&summary["membershipTier"]).unwrap_or("Open Design");
    let balance = number(&preflight["balanceUsd"]).or_else(|| number(&summary["balanceUsd"]));
    json!({"available":true,"account":{"id":account,"plan":tier},"plan":tier,"balance":balance.map(|value|format!("${value:.2}")),"items":items,
        "until":core::timestamp(&summary["subscriptionCurrentPeriodEnd"]).map(core::iso),
        "renew":if summary["subscriptionCancelAtPeriodEnd"] == true {"off"}else{"auto"},
        "message":if preflight.pointer("/codingPlan/eligible").and_then(Value::as_bool)==Some(false){Some("This account has no Design Plan allowance; requests use its wallet balance.")}else{None}})
}
#[async_trait]
impl ProviderPlugin for OpenDesign {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        self.core.initialize(params)
    }
    async fn auth_methods(&self, _: CallContext) -> PluginResult<Value> {
        Ok(self.core.config()?.manifest["contributes"]["provider"]["authMethods"].clone())
    }
    async fn authorize(&self, params: Value) -> PluginResult<Value> {
        let client = self.core.params_client(&params)?;
        match core::required(&params, "authMethodID")? {
            "apiKey" => {
                let key = core::required(core::inputs(&params), "apiKey")?;
                if key.starts_with("vela_ctrl_") {
                    return Err(PluginError::invalid(
                        "Use the runtime API key for model requests, or sign in with the account.",
                    ));
                }
                let catalog = core::checked(
                    client.as_ref(),
                    core::json_request(HttpMethod::Get, format!("{BASE}/models"), None, Some(key))?,
                )
                .await?;
                models_of(&catalog)?;
                Ok(json!({"ok":true,"credentials":core::saved_api(key)}))
            }
            "oauth" => {
                let value = core::checked(client.as_ref(), api(HttpMethod::Post, "/api/v1/cli/device-authorizations", Some(&json!({"cliVersion":"0.1.4","host":std::env::var("HOSTNAME").unwrap_or_else(|_|"Nora".into()),"profile":"prod"})), None)?).await?;
                let device = core::required(&value, "deviceId")?;
                let secret = core::required(&value, "deviceSecret")?;
                let url = core::required(&value, "activationUrl")?;
                let parsed = url::Url::parse(url).map_err(|_| {
                    PluginError::transport("Open Design returned an invalid approval address.")
                })?;
                if parsed.scheme() != "https"
                    || !parsed.username().is_empty()
                    || parsed.password().is_some()
                {
                    return Err(PluginError::transport(
                        "Open Design returned an unsafe approval address.",
                    ));
                }
                let until =
                    core::timestamp(&value["expiresAt"]).unwrap_or_else(|| core::now() + 600_000);
                let session = self.core.begin(json!({"deviceId":device,"deviceSecret":secret,"interval":value["pollIntervalSeconds"].as_u64().unwrap_or(2).clamp(1,30)}), until).await?;
                Ok(
                    json!({"session":session,"url":url,"method":"auto","instructions":format!("Approve the Open Design CLI sign-in in your browser. Code: {}",core::text(&value["userCode"]).unwrap_or(""))}),
                )
            }
            method => Err(PluginError::unsupported(method)),
        }
    }
    async fn callback(&self, params: Value) -> PluginResult<Value> {
        let pending = self.core.take_pending(&params).await?;
        let client = self.core.params_client(&params)?;
        let path = format!(
            "/api/v1/cli/device-authorizations/{}/token",
            core::encode(core::required(&pending, "deviceId")?)
        );
        while core::now() < pending["until"].as_u64().unwrap_or(0) {
            tokio::time::sleep(Duration::from_secs(
                pending["interval"].as_u64().unwrap_or(2),
            ))
            .await;
            let answer = core::exchange(
                client.as_ref(),
                api(
                    HttpMethod::Post,
                    &path,
                    Some(&json!({"deviceSecret":core::required(&pending,"deviceSecret")?})),
                    None,
                )?,
            )
            .await;
            let Ok((status, value)) = answer else {
                continue;
            };
            if core::text(&value["runtimeKey"])
                .or_else(|| core::text(&value["user"]["runtimeKey"]))
                .is_some()
                && core::text(&value["controlKey"])
                    .or_else(|| core::text(&value["user"]["controlKey"]))
                    .is_some()
            {
                return Ok(json!({"ok":true,"credentials":auth_of(&value)?}));
            }
            if matches!(status, 401 | 403 | 404)
                || ["expired", "canceled", "cancelled", "denied", "rejected"].contains(
                    &value["status"]
                        .as_str()
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .as_str(),
                )
            {
                return Err(PluginError::authentication(
                    "Open Design sign-in was refused or expired. Start again.",
                ));
            }
        }
        Err(PluginError::authentication(
            "The Open Design sign-in code expired.",
        ))
    }
    async fn refresh(&self, _: Value) -> PluginResult<Value> {
        Ok(json!({"changed":false}))
    }
    async fn models(&self, context: CallContext) -> PluginResult<Value> {
        let Some(credentials) = &context.credentials else {
            let defaults = seed();
            let models = defaults
                .as_object()
                .into_iter()
                .flatten()
                .map(|(id, facts)| model(id, &Value::Null, facts, "preset"))
                .collect();
            return Ok(core::listing(models, "openai-completions", BASE, "preset"));
        };
        let client = self.core.client(context.network_proxy.as_ref())?;
        let value = core::checked(
            client.as_ref(),
            core::json_request(
                HttpMethod::Get,
                format!("{BASE}/models"),
                None,
                Some(runtime_key(credentials)?),
            )?,
        )
        .await?;
        Ok(core::listing(
            models_of(&value)?,
            "openai-completions",
            BASE,
            "remote",
        ))
    }
    async fn usage(&self, context: CallContext) -> PluginResult<Value> {
        let credentials = context
            .credentials
            .as_ref()
            .ok_or_else(|| PluginError::authentication("Sign in to read Open Design allowance."))?;
        let key = match control_key(credentials) {
            Ok(key) => key,
            Err(_) => {
                return Ok(
                    json!({"available":false,"reason":"Account sign-in is required to read Design Plan allowance; a runtime key can only make model requests."}),
                )
            }
        };
        let client = self.core.client(context.network_proxy.as_ref())?;
        let workspace = core::checked(
            client.as_ref(),
            api(
                HttpMethod::Get,
                "/api/v1/workspaces/current",
                None,
                Some(key),
            )?,
        )
        .await?;
        let id = core::text(&workspace["activeWorkspaceId"])
            .or_else(|| core::text(&workspace["workspaceId"]))
            .ok_or_else(|| PluginError::transport("Open Design returned no active workspace."))?;
        let preflight = core::checked(
            client.as_ref(),
            api(
                HttpMethod::Get,
                &format!("/api/v1/workspaces/{}/billing/preflight", core::encode(id)),
                None,
                Some(key),
            )?,
        )
        .await?;
        let summary = core::checked(
            client.as_ref(),
            api(HttpMethod::Get, "/api/v1/billing/summary", None, Some(key))?,
        )
        .await
        .unwrap_or(Value::Null);
        Ok(usage_of(&preflight, &summary, &context.account))
    }
    async fn fetch(&self, request: FetchRequest) -> PluginResult<FetchResponse> {
        core::validate_fetch(
            &request,
            BASE,
            &["/v1/chat/completions", "/v1/responses", "/v1/messages"],
        )?;
        let credentials = request
            .credentials
            .as_ref()
            .ok_or_else(|| PluginError::authentication("Connect an Open Design account."))?;
        let key = runtime_key(credentials)?;
        let client = self.core.client(request.network_proxy.as_ref())?;
        let mut outgoing = core::json_request(HttpMethod::Post, &request.url, None, Some(key))?;
        outgoing.body = request.decode_body()?;
        outgoing
            .headers
            .insert("content-type".into(), "application/json".into());
        for (name, value) in &request.headers {
            if ["accept", "anthropic-version", "anthropic-beta"]
                .contains(&name.to_ascii_lowercase().as_str())
            {
                outgoing.headers.insert(name.clone(), value.clone());
            }
        }
        core::relay(client.as_ref(), outgoing).await
    }
}

#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
