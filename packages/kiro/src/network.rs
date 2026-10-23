use super::session::{self, Credential};
use nora_ai::{HttpClient, HttpMethod, HttpRequest};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
pub(crate) async fn json(client: &dyn HttpClient, request: HttpRequest) -> PluginResult<Value> {
    tokio::time::timeout(std::time::Duration::from_secs(25), async {
        let response = client
            .stream(request)
            .await
            .map_err(|_| PluginError::transport("The Kiro service could not be reached."))?;
        let status = response.status;
        let body = nora_ai::collect_http_body_text_limited(response.body, 4 * 1024 * 1024 + 1)
            .await
            .map_err(|_| PluginError::transport("The Kiro response was incomplete."))?;
        if body.len() > 4 * 1024 * 1024 {
            return Err(PluginError::transport("The Kiro response is too large."));
        }
        if !(200..300).contains(&status) {
            let code = if status == 403 && body.contains("Builder ID") {
                "kiro_builder_id".into()
            } else {
                format!("kiro_http_{status}")
            };
            return Err(PluginError::new(
                code,
                format!("Kiro returned HTTP {status}."),
            ));
        }
        serde_json::from_str(&body)
            .map_err(|_| PluginError::transport("Kiro returned invalid JSON."))
    })
    .await
    .map_err(|_| PluginError::transport("The Kiro request timed out."))?
}
pub(crate) fn authorize(mut request: HttpRequest, c: &Credential) -> HttpRequest {
    request = request.with_header("authorization", format!("Bearer {}", c.access));
    if let Some(kind) = c.token_type() {
        request = request.with_header("tokentype", kind);
    }
    request
}
pub(crate) fn post(url: String, value: &Value) -> PluginResult<HttpRequest> {
    HttpRequest::new(HttpMethod::Post, url)
        .with_json_body(value)
        .map_err(|_| PluginError::invalid("The Kiro request cannot be encoded."))
}
async fn control(
    client: &dyn HttpClient,
    c: &Credential,
    region: &str,
    target: &str,
    body: &Value,
    origin: bool,
) -> PluginResult<Value> {
    let region = session::region(region)?;
    let mut url = url::Url::parse(&format!("https://management.{region}.kiro.dev/"))
        .expect("validated region URL");
    if origin {
        url.query_pairs_mut().append_pair("origin", "KIRO_CLI");
    }
    let request = post(url.into(), body)?
        .with_header("content-type", "application/x-amz-json-1.0")
        .with_header("x-amz-target", target)
        .with_header("x-amzn-codewhisperer-optout", "false");
    json(client, authorize(request, c)).await
}

async fn key_models(client: &dyn HttpClient, c: &Credential) -> PluginResult<Value> {
    let region = c.runtime_region()?;
    let mut output = json!({"models":[]});
    let mut next = None;
    for _ in 0..20 {
        let mut body = json!({"origin":"KIRO_CLI"});
        if let Some(next) = next.take() {
            body["nextToken"] = json!(next);
        }
        let page = control(
            client,
            c,
            &region,
            "KiroControlPlaneBearerService.ListAvailableModels",
            &body,
            true,
        )
        .await?;
        if let Some(models) = page["models"].as_array() {
            output["models"]
                .as_array_mut()
                .expect("model collection")
                .extend(models.iter().cloned());
        }
        if output.get("defaultModel").is_none_or(Value::is_null) {
            if let Some(default) = page.get("defaultModel") {
                output["defaultModel"] = default.clone();
            }
        }
        next = match page.get("nextToken") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) if value.is_empty() => None,
            Some(value) => Some(
                session::text(Some(value), 16 * 1024)
                    .ok_or_else(|| {
                        PluginError::transport("Kiro returned an invalid models page token.")
                    })?
                    .to_owned(),
            ),
        };
        if next.is_none() {
            break;
        }
    }
    Ok(output)
}

pub(crate) async fn management(
    client: &dyn HttpClient,
    c: &Credential,
    method: &str,
    body: Option<&Value>,
) -> PluginResult<Value> {
    if c.method == "apikey" {
        return match method {
            "Get-Usage-Limits" => {
                control(
                    client,
                    c,
                    &c.runtime_region()?,
                    "AmazonCodeWhispererService.GetUsageLimits",
                    &json!({"origin":"KIRO_CLI"}),
                    true,
                )
                .await
            }
            "List-Available-Models" => key_models(client, c).await,
            _ => Err(PluginError::unsupported(method)),
        };
    }
    let region = c.runtime_region()?;
    let mut url = url::Url::parse(&format!("https://management.{region}.kiro.dev/{method}"))
        .expect("validated region URL");
    if body.is_none() {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("origin", "KIRO_CLI")
            .append_pair("profileArn", &c.profile);
        if method == "Get-Usage-Limits" {
            query
                .append_pair("resourceType", "CREDIT")
                .append_pair("isEmailRequired", "true");
        }
    }
    let request = if let Some(body) = body {
        post(url.into(), body)?
    } else {
        HttpRequest::new(HttpMethod::Get, url.as_str())
    };
    json(
        client,
        authorize(request.with_header("accept", "application/json"), c),
    )
    .await
}
pub(crate) async fn profile(client: &dyn HttpClient, c: &Credential) -> PluginResult<String> {
    if !c.profile.is_empty() {
        c.runtime_region()?;
        return Ok(c.profile.clone());
    }
    if c.method == "apikey" {
        for region in ["us-east-1", "eu-central-1"] {
            if let Ok(value) = control(
                client,
                c,
                region,
                "AmazonCodeWhispererService.GetProfile",
                &json!({}),
                false,
            )
            .await
            {
                if let Some(profile) = session::text(value.pointer("/profile/arn"), 2048) {
                    return Ok(profile.into());
                }
            }
        }
        return Ok(String::new());
    }
    if c.builder {
        return Ok(session::BUILDER_PROFILE.into());
    }
    for region in ["us-east-1", "eu-central-1"] {
        let mut cred = c.clone();
        cred.region = region.into();
        match management(client, &cred, "List-Available-Profiles", Some(&json!({}))).await {
            Ok(value) => {
                if let Some(arn) = value["profiles"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find_map(|profile| session::text(profile.get("arn"), 2048))
                {
                    return Ok(arn.into());
                }
            }
            Err(error) if c.method == "idc" && error.code == "kiro_builder_id" => {
                return Ok(session::BUILDER_PROFILE.into());
            }
            Err(_) => {}
        }
    }
    Err(session::expired())
}
pub(crate) async fn identify(client: &dyn HttpClient, c: &mut Credential) -> PluginResult<Value> {
    c.profile = profile(client, c).await?;
    c.runtime_region()?;
    let usage = management(client, c, "Get-Usage-Limits", None).await?;
    c.email = session::text(usage.pointer("/userInfo/email"), 512)
        .unwrap_or_default()
        .into();
    c.user_id = match usage.pointer("/userInfo/userId") {
        None | Some(Value::Null) => String::new(),
        Some(value) => session::text(Some(value), 500)
            .filter(|id| value.as_str() == Some(*id) && !id.chars().any(char::is_whitespace))
            .ok_or_else(session::expired)?
            .into(),
    };
    c.plan = super::usage::plan(
        session::text(usage.pointer("/subscriptionInfo/subscriptionTitle"), 128)
            .unwrap_or_default(),
    );
    c.identity = if c.method == "apikey" {
        key_identity(&c.access)
    } else {
        c.legacy_identity()
    };
    Ok(usage)
}
pub(crate) fn key_identity(access: &str) -> String {
    session::fingerprint(&format!("kiro-api-key\0{access}"))
}
pub(crate) async fn refresh(client: &dyn HttpClient, c: &Credential) -> PluginResult<Credential> {
    if c.refresh.is_empty() {
        return Err(session::expired());
    }
    let request = match c.method.as_str() {
        "social" => post(
            format!(
                "https://prod.{}.auth.desktop.kiro.dev/refreshToken",
                session::region(&c.region)?
            ),
            &json!({"refreshToken":c.refresh}),
        )?
        .with_header("user-agent", "Kiro-Desktop/0.2.13 (darwin; arm64)"),
        "idc" if !c.client_id.is_empty() && !c.client_secret.is_empty() => post(
            format!(
                "https://oidc.{}.amazonaws.com/token",
                session::region(&c.region)?
            ),
            &json!({"clientId":c.client_id,"clientSecret":c.client_secret,"refreshToken":c.refresh,"grantType":"refresh_token"}),
        )?,
        "external-idp" => {
            let endpoint = session::token_endpoint(&c.token_endpoint)?;
            if c.client_id.is_empty() {
                return Err(session::expired());
            }
            let form = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("grant_type", "refresh_token")
                .append_pair("client_id", &c.client_id)
                .append_pair("refresh_token", &c.refresh)
                .finish();
            HttpRequest::new(HttpMethod::Post, endpoint)
                .with_header("content-type", "application/x-www-form-urlencoded")
                .with_header("accept", "application/json")
                .with_body(form.into_bytes())
        }
        _ => return Err(session::expired()),
    };
    let value = json(client, request).await?;
    let mut next = c.clone();
    let external = c.method == "external-idp";
    next.access = session::secret(value.get(if external {
        "access_token"
    } else {
        "accessToken"
    }))?;
    if let Some(refresh) = value.get(if external {
        "refresh_token"
    } else {
        "refreshToken"
    }) {
        next.refresh = session::secret(Some(refresh))?;
    }
    let seconds = value[if external { "expires_in" } else { "expiresIn" }]
        .as_i64()
        .filter(|seconds| *seconds > 0 && *seconds < 31_536_000)
        .unwrap_or(3600);
    next.expires = session::now() + seconds * 1000;
    if let Some(profile) = session::text(value.get("profileArn"), 2048) {
        next.profile = profile.into();
    }
    Ok(next)
}
#[cfg(test)]
#[path = "../test/network.test.rs"]
mod tests;
