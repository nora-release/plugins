use crate::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use sha2::{Digest, Sha256};

pub(crate) struct Flow {
    site: Deployment,
    kind: Kind,
    deadline: u64,
}
enum Kind {
    Device {
        verifier: String,
        nonce: String,
        machine: String,
    },
    Pat(String),
}

pub(crate) fn enterprise_domain_preset() -> Option<String> {
    std::env::var("QWENWORK_ENT_DOMAIN")
        .ok()
        .and_then(|value| site::enterprise_domain(&value))
}

pub(crate) async fn authorize(provider: &Qoder, params: Value) -> PluginResult<Value> {
    let method = params
        .get("authMethodID")
        .or_else(|| params.get("method"))
        .and_then(Value::as_str)
        .unwrap_or("oauth");
    let fields = params
        .get("inputs")
        .or_else(|| params.get("fields"))
        .unwrap_or(&params);
    if provider.site.is_work() && method != "oauth" {
        return Err(PluginError::unsupported(method));
    }
    let enterprise = if provider.site == Site::QwenWorkEnterprise {
        Some(
            enterprise_domain_preset()
                .or_else(|| site::enterprise_domain(text(fields, "domain")))
                .ok_or_else(|| invalid("Enter your QwenWork enterprise domain."))?,
        )
    } else {
        None
    };
    let deployment = if let Some(domain) = enterprise.as_deref() {
        provider.site.deployment(Some(domain))?
    } else {
        match method {
            "oauth" | "pat" => provider.site.deployment(None)?,
            "vpc" if provider.site == Site::China => {
                provider.site.deployment(Some(text(fields, "vpc")))?
            }
            _ => return Err(PluginError::unsupported(method)),
        }
    };
    if method == "vpc" && deployment.vpc.is_none() {
        return Err(invalid("Enter the enterprise's Qoder VPC instance."));
    }
    let (session, url, kind) = if method == "pat" {
        let pat = text(fields, "pat").trim();
        if !pat.starts_with("pt-") || pat.len() > 16384 {
            return Err(invalid(
                "Enter a Qoder Personal Access Token beginning with pt-.",
            ));
        }
        (
            id(),
            format!("{}/account/integrations", deployment.web),
            Kind::Pat(pat.into()),
        )
    } else {
        let mut raw = [0u8; 64];
        rand::rngs::OsRng.fill_bytes(&mut raw);
        let verifier = URL_SAFE_NO_PAD.encode(raw);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let nonce = uuid::Uuid::new_v4().to_string();
        let machine = uuid::Uuid::new_v4().to_string();
        let mut url = url::Url::parse(&format!("{}/device/selectAccounts", deployment.web))
            .map_err(|_| invalid("The Qoder sign-in address is invalid."))?;
        url.query_pairs_mut()
            .append_pair("challenge", &challenge)
            .append_pair("challenge_method", "S256")
            .append_pair("nonce", &nonce)
            .append_pair("machine_id", &machine)
            .append_pair("client_id", provider.site.client_id());
        if provider.site == Site::QwenWork {
            url.query_pairs_mut()
                .append_pair("redirect_uri", "qwenwork-cn://");
        }
        (
            id(),
            url.to_string(),
            Kind::Device {
                verifier,
                nonce,
                machine,
            },
        )
    };
    let mut pending = provider.pending.lock().await;
    pending.retain(|_, flow| flow.deadline > now());
    if pending.len() >= 32 {
        return Err(invalid("Too many Qoder sign-ins are waiting."));
    }
    pending.insert(
        session.clone(),
        Flow {
            site: deployment,
            kind,
            deadline: now() + 900_000,
        },
    );
    Ok(
        json!({"session":session,"url":url,"method":"auto","instructions":"Complete the Qoder sign-in in your browser."}),
    )
}
pub(crate) async fn callback(provider: &Qoder, params: Value) -> PluginResult<Value> {
    let flow = provider
        .pending
        .lock()
        .await
        .remove(text(&params, "session"))
        .ok_or_else(|| invalid("The Qoder sign-in expired. Start again."))?;
    if now() > flow.deadline {
        return Err(invalid("The Qoder sign-in timed out."));
    }
    let client = provider.params_client(&params)?;
    let credentials = match flow.kind {
        Kind::Pat(pat) => pat_login(provider.site, client.as_ref(), &flow.site, &pat, None).await?,
        Kind::Device {
            verifier,
            nonce,
            machine,
        } => {
            let device = loop {
                if now() > flow.deadline {
                    return Err(invalid("The Qoder sign-in timed out."));
                }
                let poll = openapi(
                    client.as_ref(),
                    &flow.site,
                    "/api/v1/deviceToken/poll",
                    HttpMethod::Get,
                    None,
                    None,
                    vec![
                        ("nonce", nonce.clone()),
                        ("verifier", verifier.clone()),
                        ("challenge_method", "S256".into()),
                    ],
                )
                .await;
                if let Ok((200, value)) = poll {
                    if !text(&value, "token").trim().is_empty() {
                        break value;
                    }
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            };
            let (status, job) = if provider.site.is_work() {
                (0, json!({}))
            } else {
                openapi(
                    client.as_ref(),
                    &flow.site,
                    "/api/v1/me/jobToken",
                    HttpMethod::Post,
                    Some(text(&device, "token")),
                    Some(json!({"clientId":provider.site.client_id()})),
                    vec![],
                )
                .await?
            };
            let device_chat = provider.site.is_work()
                || status >= 400 && status < 500 && provider.site == Site::China;
            let (access, refresh, expires) = if status == 200 && !text(&job, "token").is_empty() {
                (
                    text(&job, "token"),
                    text(&job, "refresh_token"),
                    job_expiry(&job),
                )
            } else if device_chat {
                (
                    text(&device, "token"),
                    text(&device, "refresh_token"),
                    device_expiry(&device),
                )
            } else {
                return Err(transport(format!(
                    "Qoder job token exchange failed (HTTP {status})."
                )));
            };
            let mut value = json!({"type":"oauth","access":access,"refresh":refresh,"expires":expires,"machineId":machine,"deviceToken":device.get("token"),"deviceRefresh":text(&device,"refresh_token"),"uid":identifier(device.get("user_id").unwrap_or(&Value::Null)),"name":text(&device,"user_name"),"email":"","deviceChat":device_chat});
            if let Ok((200, profile)) = openapi(
                client.as_ref(),
                &flow.site,
                provider.site.user_info(),
                HttpMethod::Get,
                Some(text(&device, "token")),
                None,
                vec![],
            )
            .await
            {
                let profile = if provider.site.is_work() {
                    profile
                        .pointer("/data/user")
                        .or_else(|| profile.get("user"))
                        .unwrap_or(&profile)
                } else {
                    &profile
                };
                value["email"] = json!(text(profile, "email"));
                if !text(&profile, "name").is_empty() {
                    value["name"] = profile["name"].clone();
                }
                if text(&value, "uid").is_empty() || provider.site.is_work() {
                    let uid = profile_id(profile);
                    if !uid.is_empty() {
                        value["uid"] = json!(uid);
                    }
                }
            }
            if provider.site == Site::QwenWorkEnterprise {
                value["enterpriseHost"] = json!(flow.site.vpc);
            }
            finish_identity(&mut value, &flow.site)?;
            checked(&value)?;
            value
        }
    };
    Ok(json!({"ok":true,"credentials":credentials}))
}
fn identifier(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|value| value.to_string()))
        .unwrap_or_default()
}
fn profile_id(value: &Value) -> String {
    ["id", "user_id", "uid"]
        .into_iter()
        .map(|key| identifier(value.get(key).unwrap_or(&Value::Null)))
        .find(|id| !id.is_empty())
        .unwrap_or_default()
}
fn finish_identity(value: &mut Value, deployment: &Deployment) -> PluginResult<()> {
    if text(value, "uid").is_empty() {
        return Err(authentication(
            "Qoder did not identify this account. Sign in again.",
        ));
    }
    let identity = if text(value, "email").is_empty() {
        text(value, "uid")
    } else {
        text(value, "email")
    }
    .to_owned();
    value["accountId"] = json!(if let Some(instance) = &deployment.vpc {
        format!("{identity} ({instance})")
    } else {
        identity
    });
    if let Some(instance) = &deployment.vpc {
        value["vpc"] = json!(instance);
    }
    crate::identity::assign(value, deployment)?;
    Ok(())
}
async fn pat_login(
    site: Site,
    client: &dyn HttpClient,
    deployment: &Deployment,
    pat: &str,
    previous: Option<&Value>,
) -> PluginResult<Value> {
    let (status, token) = openapi(
        client,
        deployment,
        "/api/v1/jobToken/exchange",
        HttpMethod::Post,
        None,
        Some(json!({"personal_token":pat})),
        vec![],
    )
    .await?;
    if [400, 401, 403].contains(&status) {
        return Err(authentication(
            "Qoder refused this Personal Access Token. Sign in with a current token.",
        ));
    }
    if status != 200 {
        return Err(transport(format!(
            "Qoder PAT exchange failed (HTTP {status})."
        )));
    }
    let access = ["token", "device_token", "access_token"]
        .into_iter()
        .map(|key| text(&token, key))
        .find(|value| !value.trim().is_empty())
        .ok_or_else(|| transport("Qoder PAT exchange returned no job token."))?;
    let refresh = token
        .get("refresh_token")
        .or_else(|| token.get("refreshToken"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let (status, profile) = openapi(
        client,
        deployment,
        "/api/v1/userinfo",
        HttpMethod::Get,
        Some(access),
        None,
        vec![],
    )
    .await?;
    if status != 200 || profile_id(&profile).is_empty() {
        return Err(transport("Qoder PAT sign-in did not identify the account."));
    }
    let mut value = previous.cloned().unwrap_or_else(|| json!({}));
    value["type"] = json!("oauth");
    value["access"] = json!(access);
    value["refresh"] = json!(refresh);
    value["expires"] = json!(exchange_expiry(&token));
    value["uid"] = json!(profile_id(&profile));
    value["email"] = json!(text(&profile, "email"));
    value["name"] = json!(text(&profile, "name"));
    value["patAuth"] = json!(true);
    value["pat"] = json!(pat);
    if previous.is_none() {
        value["machineId"] = json!(uuid::Uuid::new_v4().to_string());
        value["deviceToken"] = json!("");
        value["deviceRefresh"] = json!("");
    }
    finish_identity(&mut value, deployment)?;
    let _ = site;
    Ok(value)
}
pub(crate) async fn renew(
    provider: &Qoder,
    client: &dyn HttpClient,
    previous: &Value,
    device_only: bool,
) -> PluginResult<Value> {
    let deployment = provider.deployment(previous)?;
    let mut cache = provider.spent.lock().await;
    cache.retain(|_, (_, at)| now().saturating_sub(*at) < 3_600_000);
    let current = latest(&cache, &deployment, previous, device_only);
    if current != *previous
        && (device_only || current["expires"].as_u64().unwrap_or(0) > now().saturating_add(600_000))
    {
        return Ok(current);
    }
    let previous = &current;
    let device_chat = previous.get("deviceChat").and_then(Value::as_bool) == Some(true);
    let device = device_only || device_chat;
    let refresh = text(
        previous,
        if device_only && !device_chat {
            "deviceRefresh"
        } else {
            "refresh"
        },
    );
    let cache_key = codec_key(&deployment, refresh, device, text(previous, "access"));
    let result = if refresh.is_empty() {
        Err(authentication("This Qoder sign-in has no refresh token."))
    } else {
        let path = if device {
            "/api/v1/deviceToken/refresh"
        } else {
            "/api/v1/jobToken/refresh"
        };
        let (status, value) = openapi(
            client,
            &deployment,
            path,
            HttpMethod::Post,
            None,
            Some(if provider.site.is_work() {
                json!({"refresh_token":refresh,"target":"c"})
            } else {
                json!({"refresh_token":refresh})
            }),
            vec![],
        )
        .await?;
        if status == 401 || status == 403 {
            Err(authentication(if device_only && !device_chat {
                "Qoder refused the account-page sign-in. Chat can continue; sign in again to restore usage."
            } else {
                "Qoder refused the refresh token. Sign in again."
            }))
        } else if status != 200 {
            Err(transport(format!(
                "Qoder token renewal failed (HTTP {status})."
            )))
        } else {
            let token = if text(&value, "token").trim().is_empty() && device {
                text(&value, "device_token")
            } else {
                text(&value, "token")
            };
            let refresh = text(&value, "refresh_token");
            if token.trim().is_empty() || refresh.trim().is_empty() {
                Err(transport(
                    "Qoder token renewal returned an incomplete pair.",
                ))
            } else {
                let mut next = previous.clone();
                if !device_only || device_chat {
                    next["access"] = json!(token);
                    next["refresh"] = json!(refresh);
                    next["expires"] = json!(if device {
                        device_expiry(&value)
                    } else {
                        job_expiry(&value)
                    });
                }
                if device {
                    next["deviceToken"] = json!(token);
                    next["deviceRefresh"] = json!(refresh);
                }
                Ok(next)
            }
        }
    };
    let next = match result {
        Ok(next) => next,
        Err(error)
            if !device
                && error.code == "authentication_required"
                && previous.get("patAuth").and_then(Value::as_bool) == Some(true)
                && !text(previous, "pat").is_empty() =>
        {
            pat_login(
                provider.site,
                client,
                &deployment,
                text(previous, "pat"),
                Some(previous),
            )
            .await?
        }
        Err(error) => return Err(error),
    };
    if cache.len() >= 256 {
        if let Some(first) = cache
            .iter()
            .min_by_key(|(_, (_, at))| at)
            .map(|(key, _)| key.clone())
        {
            cache.remove(&first);
        }
    }
    cache.insert(cache_key, (next.clone(), now()));
    Ok(next)
}

fn latest(
    cache: &BTreeMap<String, (Value, u64)>,
    deployment: &Deployment,
    previous: &Value,
    device_only: bool,
) -> Value {
    let mut current = previous.clone();
    for _ in 0..16 {
        let device_chat = current["deviceChat"] == true;
        let refresh = text(
            &current,
            if device_only && !device_chat {
                "deviceRefresh"
            } else {
                "refresh"
            },
        );
        let key = codec_key(
            deployment,
            refresh,
            device_only || device_chat,
            text(&current, "access"),
        );
        let Some((next, _)) = cache
            .get(&key)
            .filter(|(_, at)| now().saturating_sub(*at) < 3_600_000)
        else {
            break;
        };
        if next == &current {
            break;
        }
        current = next.clone();
    }
    current
}

pub(crate) async fn fresh(
    provider: &Qoder,
    client: &dyn HttpClient,
    previous: &Value,
) -> PluginResult<Value> {
    let deployment = provider.deployment(previous)?;
    let current = {
        let cache = provider.spent.lock().await;
        latest(&cache, &deployment, previous, false)
    };
    let next = if current["expires"].as_u64().unwrap_or(0) > now().saturating_add(300_000) {
        current
    } else {
        renew(provider, client, &current, false).await?
    };
    if next != *previous {
        nora_provider_plugin_sdk::save_credentials(next.clone()).await?;
    }
    Ok(next)
}
fn codec_key(deployment: &Deployment, refresh: &str, device: bool, access: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(
            format!(
                "{}\0{device}\0{}",
                deployment.openapi,
                if refresh.is_empty() { access } else { refresh }
            )
            .as_bytes()
        )
    )
}
fn job_expiry(value: &Value) -> u64 {
    now().saturating_add(
        value
            .get("expires_in")
            .and_then(number)
            .filter(|value| *value > 0.0)
            .unwrap_or(86_400_000.0) as u64,
    )
}
fn device_expiry(value: &Value) -> u64 {
    value
        .get("expires_at")
        .and_then(usage::date)
        .unwrap_or_else(|| {
            now().saturating_add(
                (value
                    .get("expires_in")
                    .and_then(number)
                    .filter(|value| *value > 0.0)
                    .unwrap_or(86400.0)
                    * 1000.0) as u64,
            )
        })
}
fn exchange_expiry(value: &Value) -> u64 {
    ["expire_time", "expires_at"]
        .into_iter()
        .find_map(|key| value.get(key).and_then(usage::date))
        .unwrap_or_else(|| job_expiry(value))
}
#[cfg(test)]
#[path = "../test/auth.test.rs"]
mod tests;
