use super::*;
use nora_ai::HttpClient;

#[derive(Clone, Debug, Default)]
pub(crate) struct Project {
    pub id: String,
    pub plan: String,
    pub tier: String,
    pub paid: String,
}
impl Project {
    pub fn entitlements(&self) -> Vec<String> {
        let mut values = Vec::new();
        for tier in [&self.paid, &self.tier] {
            if !tier.is_empty()
                && !["free-tier", "legacy-tier"].contains(&tier.as_str())
                && !values.contains(tier)
            {
                values.push(tier.clone());
            }
        }
        values.push(String::new());
        values
    }
}
fn id(value: &Value) -> String {
    text(Some(value), 512)
        .or_else(|| text(value.get("id"), 512))
        .unwrap_or_default()
        .into()
}
fn plan(value: &Value) -> String {
    ["paidTier", "currentTier"]
        .iter()
        .find_map(|key| {
            text(value[*key].get("name"), 512).or_else(|| text(value[*key].get("id"), 512))
        })
        .unwrap_or_default()
        .into()
}
fn needs_project() -> PluginError {
    invalid("This Gemini CLI account needs a Google Cloud project with Code Assist enabled. Reconnect and enter its project ID.")
}

pub(crate) async fn load(
    app: App,
    tokens: &session::Tokens,
    client: &dyn HttpClient,
) -> PluginResult<Project> {
    let mut body = if app == App::Antigravity {
        json!({"metadata":{"ideType":"ANTIGRAVITY"}})
    } else {
        json!({"metadata":{"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}})
    };
    if app == App::GeminiCLI && !tokens.project.is_empty() {
        body["metadata"]["duetProject"] = json!(tokens.project);
        body["cloudaicompanionProject"] = json!(tokens.project);
    }
    let payload = auth::json_request(
        client,
        auth::authorized(app, tokens, PROD, "loadCodeAssist", &body)?,
    )
    .await;
    if app == App::Antigravity && !tokens.project.is_empty() && payload.is_err() {
        return Ok(Project {
            id: tokens.project.clone(),
            ..Default::default()
        });
    }
    let payload = payload?;
    let current = payload.get("currentTier").filter(|value| value.is_object());
    let loaded = id(&payload["cloudaicompanionProject"]);
    let mut project = Project {
        id: if app == App::GeminiCLI && !loaded.is_empty() {
            loaded
        } else if !tokens.project.is_empty() {
            tokens.project.clone()
        } else {
            loaded
        },
        plan: plan(&payload),
        tier: id(&payload["currentTier"]),
        paid: id(&payload["paidTier"]),
    };
    if app == App::Antigravity || current.is_some() {
        if !project.id.is_empty() {
            return Ok(project);
        }
    }
    if app == App::GeminiCLI && current.is_some() {
        return Err(needs_project());
    }
    let tiers = payload.get("allowedTiers").and_then(Value::as_array);
    let default = tiers.and_then(|tiers| {
        tiers
            .iter()
            .find(|tier| tier["isDefault"] == true)
            .or_else(|| tiers.first())
    });
    let mut tier = default
        .map(|value| id(&value["id"]))
        .unwrap_or_else(|| "legacy-tier".into());
    if app == App::GeminiCLI
        && default.is_none_or(|tier| tier["userDefinedCloudaicompanionProject"] == true)
        && tokens.project.is_empty()
    {
        return Err(needs_project());
    }
    let request = if app == App::Antigravity {
        if tier.is_empty() || tier == "legacy-tier" {
            tier = "free-tier".into();
        }
        json!({"tier_id":tier,"metadata":{"ide_type":"ANTIGRAVITY","ide_version":if tokens.version.is_empty(){"2.9.1"}else{&tokens.version},"ide_name":"antigravity"}})
    } else {
        let mut request = json!({"tierId":tier,"metadata":body["metadata"]});
        if tier != "free-tier" {
            request["cloudaicompanionProject"] = json!(tokens.project);
        }
        request
    };
    for attempt in 0..5 {
        let mut request = auth::authorized(app, tokens, app.base(), "onboardUser", &request)?;
        if app == App::Antigravity {
            request.headers.insert(
                "user-agent".into(),
                format!(
                    "{} google-api-nodejs-client/10.3.0",
                    app.user_agent_with_version(
                        "",
                        if tokens.version.is_empty() {
                            "2.9.1"
                        } else {
                            &tokens.version
                        }
                    )
                ),
            );
            request
                .headers
                .insert("x-goog-api-client".into(), "gl-node/22.21.1".into());
        }
        let reply = auth::json_request(client, request).await?;
        if reply["done"] == true {
            project.id = id(&reply["response"]["cloudaicompanionProject"]);
            if project.id.is_empty() {
                project.id = tokens.project.clone();
            }
            if project.id.is_empty() {
                return Err(if app == App::GeminiCLI {
                    needs_project()
                } else {
                    invalid("Antigravity did not provide a project. Sign in to the Antigravity app once, then reconnect.")
                });
            }
            project.plan = default
                .and_then(|value| text(value.get("name"), 512))
                .unwrap_or(&tier)
                .into();
            project.tier = tier;
            return Ok(project);
        }
        if attempt < 4 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }
    Err(PluginError::transport(
        "Code Assist account setup is still pending. Try again later.",
    ))
}

#[cfg(test)]
#[path = "../test/project.test.rs"]
mod tests;
