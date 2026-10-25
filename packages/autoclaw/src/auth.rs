use super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

pub(super) const SESSION_MS: u64 = 600_000;
pub(super) enum Flow {
    Sms {
        device: String,
        phone: String,
        client: Arc<dyn HttpClient>,
    },
    Browser {
        result: tokio::sync::oneshot::Receiver<PluginResult<Value>>,
        task: tokio::task::AbortHandle,
    },
}
pub(super) struct Pending {
    pub deadline: u64,
    pub flow: Flow,
}
impl Drop for Pending {
    fn drop(&mut self) {
        if let Flow::Browser { task, .. } = &self.flow {
            task.abort();
        }
    }
}
pub(super) fn bare(value: &str) -> &str {
    let mut value = value.trim();
    while value
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("bearer "))
    {
        value = value[7..].trim();
    }
    value
}
fn claim(token: &str, key: &str) -> Option<Value> {
    let part = bare(token).split('.').nth(1)?;
    let value: Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part.trim_end_matches('=')).ok()?).ok()?;
    value.get(key).cloned()
}
pub(super) fn tokens(data: &Value, old: &Value, site: Site, at: u64) -> PluginResult<Value> {
    let access = bare(text(data, "access_token"));
    let refresh = bare(
        data.get("refresh_token")
            .and_then(Value::as_str)
            .unwrap_or(text(old, "refresh")),
    );
    if access.is_empty()
        || refresh.is_empty()
        || access.len() > 65536
        || refresh.len() > 65536
        || access.chars().any(char::is_control)
        || refresh.chars().any(char::is_control)
    {
        return Err(signed_out());
    }
    let expiry = claim(access, "exp")
        .as_ref()
        .and_then(Value::as_u64)
        .and_then(|n| n.checked_mul(1000))
        .filter(|n| *n > 0)
        .unwrap_or_else(|| {
            at.saturating_add(
                data["expires_in"]
                    .as_u64()
                    .filter(|n| *n > 0 && *n <= 366 * 86400)
                    .unwrap_or(5 * 3600)
                    * 1000,
            )
        });
    let mut result = old
        .as_object()
        .cloned()
        .map(Value::Object)
        .unwrap_or(json!({}));
    for (key, value) in [
        ("type", json!("oauth")),
        ("authMethod", json!("oauth")),
        ("provider", json!(site.id())),
        ("access", json!(access)),
        ("refresh", json!(refresh)),
        ("expires", json!(expiry.saturating_sub(600_000))),
    ] {
        result[key] = value;
    }
    if let Some(uid) = data
        .get("user_id")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    {
        result["uid"] = json!(uid);
    }
    identity(site, &mut result)?;
    Ok(result)
}
pub(super) fn identity(site: Site, credentials: &mut Value) -> PluginResult<bool> {
    let Some(uid) = credentials
        .get("uid")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return Ok(false);
    };
    if uid.trim() != uid || uid.len() > 512 || uid.chars().any(char::is_control) {
        return Err(signed_out());
    }
    let value = json!({"issuer":site.host(),"subject":uid});
    let changed = credentials.get("accountIdentity") != Some(&value);
    credentials["accountIdentity"] = value;
    Ok(changed)
}
pub(super) async fn profile(
    client: &dyn HttpClient,
    site: Site,
    credentials: &mut Value,
    fallback: &str,
) {
    let response = transport::call(
        client,
        site.host(),
        "/userapi/v1/user-profile",
        HttpMethod::Post,
        text(credentials, "access"),
        Some(json!({"source_id":"autoclaw","device_id":"nora-plugin"})),
    )
    .await
    .ok()
    .filter(|r| r.code == 0);
    if let Some(response) = response {
        let data = &response.data;
        if let Some(uid) = data
            .get("user_id")
            .and_then(Value::as_str)
            .filter(|v| !v.trim().is_empty())
        {
            credentials["uid"] = json!(uid);
        }
        let name = ["user_name", "user_phone", "user_id"]
            .into_iter()
            .find_map(|key| {
                data.get(key).and_then(Value::as_str).filter(|v| {
                    !v.trim().is_empty() && v.len() <= 512 && !v.chars().any(char::is_control)
                })
            })
            .unwrap_or(fallback);
        credentials["name"] = json!(name);
        if let Some(email) = data
            .get("user_email")
            .or_else(|| data.get("email"))
            .and_then(Value::as_str)
            .filter(|v| v.contains('@') && v.len() <= 512 && !v.chars().any(char::is_control))
        {
            credentials["email"] = json!(email);
        }
    } else if text(credentials, "name").is_empty() {
        credentials["name"] = json!(fallback);
    }
    let _ = identity(site, credentials);
}
pub(super) async fn renew(
    client: &dyn HttpClient,
    site: Site,
    credentials: &Value,
) -> PluginResult<Value> {
    let refresh = bare(text(credentials, "refresh"));
    if refresh.is_empty() {
        return Err(signed_out());
    }
    let device = claim(refresh, "device_id")
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    let sms = claim(refresh, "source_id")
        .and_then(|v| {
            v.as_str()
                .map(|s| s.to_ascii_lowercase().starts_with("agent"))
        })
        .unwrap_or(false);
    let first = if sms {
        Site::China
    } else {
        Site::International
    };
    let body = json!({"source_id":"autoclaw","device_id":device,"refresh_token":refresh});
    for host in [first, first.other()] {
        let mut result = transport::call(
            client,
            host.host(),
            "/userapi/v1/refresh",
            HttpMethod::Post,
            "",
            Some(body.clone()),
        )
        .await?;
        if result.code == 400002 {
            result = transport::call(
                client,
                host.host(),
                "/userapi/v1/agent-refresh",
                HttpMethod::Post,
                "",
                Some(body.clone()),
            )
            .await?;
        }
        if result.code == 400000 {
            continue;
        }
        if result.code != 0 {
            return Err(if result.code == 401001 {
                signed_out()
            } else {
                PluginError::transport("AutoClaw could not refresh this sign-in.")
            });
        }
        return tokens(&result.data, credentials, site, now());
    }
    Err(signed_out())
}
pub(super) async fn insert(provider: &Autoclaw, pending: Pending) -> PluginResult<String> {
    let mut entries = provider.pending.lock().await;
    entries.retain(|_, pending| pending.deadline > now());
    if entries.len() >= 32 {
        return Err(invalid("Too many AutoClaw sign-ins are waiting."));
    }
    let id = random_hex(16)?;
    entries.insert(id.clone(), pending);
    Ok(id)
}
pub(super) async fn authorize(provider: &Autoclaw, params: Value) -> PluginResult<Value> {
    let method = params
        .get("authMethodID")
        .or_else(|| params.get("method"))
        .and_then(Value::as_str)
        .unwrap_or("oauth");
    if method != "oauth" {
        return Err(PluginError::unsupported(method));
    }
    let input = params
        .get("inputs")
        .or_else(|| params.get("fields"))
        .unwrap_or(&params);
    let client = provider.params_client(&params)?;
    if provider.site == Site::International {
        let vendor = match input
            .get("vendor")
            .and_then(Value::as_str)
            .unwrap_or("google")
        {
            "google" => "google",
            "zai" => "zai",
            _ => return Err(invalid("Choose Google or z.ai for the AutoClaw sign-in.")),
        };
        return guide::begin(
            provider,
            client,
            vendor,
            &[18432, 19654, 19723, 53699],
            Duration::from_secs(600),
        )
        .await;
    }
    let phone = text(input, "phone").trim();
    if phone.len() != 11 || !phone.starts_with('1') || !phone.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid("Enter an 11-digit China phone number without +86."));
    }
    let device = random_hex(32)?;
    let response = transport::call(
        client.as_ref(),
        Site::China.host(),
        "/userapi/v1/agent-send-code",
        HttpMethod::Post,
        "",
        Some(json!({"source_id":"autoclaw","device_id":device,"phone":phone})),
    )
    .await?;
    if response.code != 0 {
        return Err(PluginError::transport(
            "AutoClaw could not send the verification code.",
        ));
    }
    let session = insert(
        provider,
        Pending {
            deadline: now() + SESSION_MS,
            flow: Flow::Sms {
                device,
                phone: phone.into(),
                client,
            },
        },
    )
    .await?;
    Ok(
        json!({"session":session,"method":"code","url":"","instructions":format!("Enter the verification code sent to {}****{}.",&phone[..3],&phone[7..])}),
    )
}
pub(super) async fn callback(provider: &Autoclaw, params: Value) -> PluginResult<Value> {
    provider.require_subscriptions(
        params
            .get("subscriptionAccountsEnabled")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    )?;
    let id = text(&params, "session");
    let mut pending = provider
        .pending
        .lock()
        .await
        .remove(id)
        .filter(|flow| flow.deadline > now())
        .ok_or_else(|| {
            invalid("The AutoClaw sign-in expired or was cancelled. Begin a new sign-in.")
        })?;
    let credentials = match &mut pending.flow {
        Flow::Browser { result, .. } => tokio::time::timeout(Duration::from_secs(300), result)
            .await
            .map_err(|_| invalid("The AutoClaw sign-in timed out."))?
            .map_err(|_| invalid("The AutoClaw sign-in stopped."))??,
        Flow::Sms {
            device,
            phone,
            client,
        } => {
            let code = params
                .get("code")
                .or_else(|| params.get("input"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if code.is_empty() || code.len() > 16 || code.chars().any(char::is_control) {
                return Err(invalid(
                    "Enter the verification code received from AutoClaw.",
                ));
            }
            let value = code
                .parse::<u64>()
                .map(|n| json!(n))
                .unwrap_or_else(|_| json!(code));
            let response = transport::call(
                client.as_ref(),
                Site::China.host(),
                "/userapi/v1/agent-login/",
                HttpMethod::Post,
                "",
                Some(json!({"source_id":"autoclaw","device_id":device,"phone":phone,"code":value})),
            )
            .await?;
            if response.code != 0 {
                return Err(signed_out());
            }
            let mut credentials = tokens(&response.data, &Value::Null, provider.site, now())?;
            profile(client.as_ref(), provider.site, &mut credentials, phone).await;
            credentials
        }
    };
    Ok(json!({"credentials":credentials}))
}
pub(super) async fn cancel(provider: &Autoclaw, params: &Value) -> PluginResult<Value> {
    let removed = provider
        .pending
        .lock()
        .await
        .remove(text(params, "session"))
        .is_some();
    Ok(json!({"cancelled":removed}))
}

#[cfg(test)]
#[path = "../test/auth.test.rs"]
mod tests;
