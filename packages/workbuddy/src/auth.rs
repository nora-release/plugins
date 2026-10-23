use super::*;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub(super) struct Pending {
    state: String,
    deadline: u64,
    client: Arc<dyn HttpClient>,
}

pub(super) async fn authorize(provider: &WorkBuddy, params: Value) -> PluginResult<Value> {
    let method = params
        .get("method")
        .or_else(|| params.get("authMethodID"))
        .and_then(Value::as_str)
        .unwrap_or("oauth");
    if method == "desktop" {
        let path = default_source(provider.site)?;
        let account = read_file(&path)?;
        return Ok(
            json!({"ok":true,"credentials":source_credentials(provider.site, &path, &account)?}),
        );
    }
    if method != "oauth" {
        return Err(PluginError::unsupported(method));
    }
    let client = provider.params_client(&params)?;
    let data = provider
        .call(
            client.as_ref(),
            HttpMethod::Post,
            &format!("/v2/plugin/auth/state?platform={}", provider.site.id()),
            BTreeMap::from([
                ("x-no-authorization".into(), "true".into()),
                ("x-no-user-id".into(), "true".into()),
                ("x-no-enterprise-id".into(), "true".into()),
                ("x-no-department-info".into(), "true".into()),
            ]),
            Some(json!({})),
        )
        .await?;
    let state = text(&data, "state");
    if state.is_empty() || state.len() > 8192 {
        return Err(invalid("WorkBuddy did not provide a valid login state."));
    }
    let mut url = url::Url::parse(text(&data, "authUrl"))
        .map_err(|_| invalid("WorkBuddy did not provide a login URL."))?;
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("WorkBuddy provided an unsafe login URL."));
    }
    url.query_pairs_mut()
        .append_pair("version", "2.0.0")
        .append_pair("loginSessionId", &random_id());
    let session = random_id();
    let mut pending = provider.pending.lock().await;
    pending.retain(|_, pending| pending.deadline > now());
    if pending.len() >= 32 {
        return Err(invalid("Too many WorkBuddy sign-ins are waiting."));
    }
    pending.insert(
        session.clone(),
        Pending {
            state: state.into(),
            deadline: now() + 300_000,
            client,
        },
    );
    Ok(
        json!({"session":session,"url":url.to_string(),"method":"auto","instructions":"Complete the WorkBuddy sign-in in your browser."}),
    )
}

pub(super) async fn callback(provider: &WorkBuddy, params: Value) -> PluginResult<Value> {
    let session = text(&params, "session");
    let pending = provider
        .pending
        .lock()
        .await
        .remove(session)
        .ok_or_else(|| invalid("The WorkBuddy sign-in expired."))?;
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("state", &pending.state)
        .finish();
    let token = poll(
        provider,
        &pending,
        &format!("/v2/plugin/auth/token?{query}"),
        BTreeMap::new(),
        11217,
    )
    .await?;
    let mut credentials = merge(&json!({"type":"oauth"}), &token)?;
    let who = poll(
        provider,
        &pending,
        &format!("/v2/plugin/login/account?{query}"),
        BTreeMap::from([
            (
                "authorization".into(),
                format!("Bearer {}", text(&credentials, "access")),
            ),
            ("x-domain".into(), domain(provider.site, &credentials)),
            ("x-no-user-id".into(), "true".into()),
            ("x-no-enterprise-id".into(), "true".into()),
        ]),
        12151,
    )
    .await?;
    let uid = text(&who, "uid");
    if uid.is_empty() || uid.len() > 1024 || uid.chars().any(char::is_control) {
        return Err(invalid("WorkBuddy returned no account identity."));
    }
    let enterprise = enterprise(provider, pending.client.as_ref(), &credentials, &who).await?;
    credentials["uid"] = json!(uid);
    credentials["accountId"] = json!(uid);
    credentials["name"] = json!(["nickname", "phoneNumber", "uid"]
        .into_iter()
        .map(|key| text(&who, key))
        .find(|text| !text.is_empty())
        .unwrap_or(uid));
    if !enterprise.is_empty() {
        credentials["enterpriseId"] = json!(enterprise);
    }
    assign_identity(provider.site, &mut credentials)?;
    Ok(json!({"ok":true,"credentials":credentials}))
}

async fn poll(
    provider: &WorkBuddy,
    pending: &Pending,
    path: &str,
    headers: BTreeMap<String, String>,
    retry: i64,
) -> PluginResult<Value> {
    loop {
        if now() >= pending.deadline {
            return Err(invalid("The WorkBuddy browser sign-in timed out."));
        }
        match provider
            .call(
                pending.client.as_ref(),
                HttpMethod::Get,
                path,
                headers.clone(),
                None,
            )
            .await
        {
            Ok(value) if value.as_object().is_some_and(|object| !object.is_empty()) => {
                return Ok(value)
            }
            Ok(_) => {}
            Err(error)
                if error.code == format!("workbuddy_{retry}")
                    || error.message.contains("HTTP 408,")
                    || error.message.contains("HTTP 429,") => {}
            Err(error) => return Err(error),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn enterprise(
    provider: &WorkBuddy,
    client: &dyn HttpClient,
    credentials: &Value,
    who: &Value,
) -> PluginResult<String> {
    if !text(who, "enterpriseId").trim().is_empty() {
        return Ok(text(who, "enterpriseId").trim().into());
    }
    if text(who, "type").is_empty() || text(who, "type").eq_ignore_ascii_case("personal") {
        return Ok(String::new());
    }
    let accounts = provider
        .call(
            client,
            HttpMethod::Get,
            "/v2/plugin/accounts",
            BTreeMap::from([
                (
                    "authorization".into(),
                    format!("Bearer {}", text(credentials, "access")),
                ),
                ("x-domain".into(), domain(provider.site, credentials)),
            ]),
            None,
        )
        .await;
    let Ok(accounts) = accounts else {
        return Ok(String::new());
    };
    let mine = accounts
        .get("accounts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|account| {
            text(account, "uid") == text(who, "uid")
                && !text(account, "enterpriseId").trim().is_empty()
        })
        .collect::<Vec<_>>();
    let same = mine
        .iter()
        .copied()
        .filter(|account| text(account, "type").eq_ignore_ascii_case(text(who, "type")))
        .collect::<Vec<_>>();
    let last = mine
        .iter()
        .copied()
        .filter(|account| account.get("lastLogin").and_then(Value::as_bool) == Some(true))
        .collect::<Vec<_>>();
    Ok(if same.len() == 1 {
        text(same[0], "enterpriseId")
    } else if last.len() == 1 {
        text(last[0], "enterpriseId")
    } else if mine.len() == 1 {
        text(mine[0], "enterpriseId")
    } else {
        ""
    }
    .trim()
    .into())
}

pub(super) fn merge(previous: &Value, token: &Value) -> PluginResult<Value> {
    let access = text(token, "accessToken");
    if access.is_empty() || access.len() > 256 * 1024 || access.chars().any(char::is_control) {
        return Err(invalid("WorkBuddy did not return an access token."));
    }
    let mut next = previous.clone();
    next["access"] = json!(access);
    for (field, key) in [
        ("refresh", "refreshToken"),
        ("domain", "domain"),
        ("tokenType", "tokenType"),
    ] {
        let value = text(token, key);
        if !value.is_empty() {
            if value.chars().any(char::is_control) {
                return Err(invalid("WorkBuddy returned invalid token metadata."));
            }
            next[field] = json!(value);
        }
    }
    for (field, absolute, relative) in [
        ("expires", "expiresAt", "expiresIn"),
        ("refreshExpiresAt", "refreshExpiresAt", "refreshExpiresIn"),
    ] {
        let absolute = token
            .get(absolute)
            .and_then(number)
            .filter(|value| *value > 0.0);
        let until = absolute.or_else(|| {
            token
                .get(relative)
                .and_then(number)
                .filter(|value| *value > 0.0)
                .map(|seconds| now() as f64 + seconds * 1000.0)
        });
        if let Some(until) = until {
            next[field] = json!(until as u64);
        }
    }
    Ok(next)
}

pub(super) async fn renew(
    provider: &WorkBuddy,
    client: &dyn HttpClient,
    credentials: &Value,
) -> PluginResult<Value> {
    let refresh = text(credentials, "refresh");
    if refresh.is_empty()
        || credentials
            .get("refreshExpiresAt")
            .and_then(Value::as_u64)
            .is_some_and(|expires| expires > 0 && expires <= now())
    {
        return Err(signed_out());
    }
    let key = format!("{:x}", Sha256::digest(refresh.as_bytes()));
    // A single process never spends the same refresh token twice. Successful
    // rotations remain available to requests that entered with the old pair.
    let mut refreshed = provider.refreshed.lock().await;
    refreshed.retain(|_, (_, at)| now().saturating_sub(*at) < 60_000);
    if let Some((next, _)) = refreshed.get(&key) {
        return Ok(next.clone());
    }
    let mut headers = enterprise_headers(credentials);
    headers.extend([
        ("x-refresh-token".into(), refresh.into()),
        ("x-auth-refresh-source".into(), "plugin".into()),
        ("x-domain".into(), domain(provider.site, credentials)),
    ]);
    let token = provider
        .call(
            client,
            HttpMethod::Post,
            "/v2/plugin/auth/token/refresh",
            headers,
            Some(json!({})),
        )
        .await?;
    let next = merge(credentials, &token)?;
    if refreshed.len() >= 128 {
        if let Some(oldest) = refreshed
            .iter()
            .min_by_key(|(_, (_, at))| *at)
            .map(|(key, _)| key.clone())
        {
            refreshed.remove(&oldest);
        }
    }
    refreshed.insert(key, (next.clone(), now()));
    Ok(next)
}

pub(super) async fn current(
    provider: &WorkBuddy,
    client: &dyn HttpClient,
    credentials: &Value,
) -> PluginResult<Value> {
    if credentials.get("source").and_then(Value::as_str) != Some("desktop") {
        if text(credentials, "access").is_empty() {
            return Err(signed_out());
        }
        return Ok(credentials.clone()); // Nora renews browser accounts before dispatch.
    }
    let source = read_source(provider.site, credentials)?;
    let mut held = provider.source_tokens.lock().await;
    let source_account = format!(
        "{}\0{}",
        text(&source, "uid"),
        text(&source, "enterpriseId")
    );
    let previous = held
        .get(&source_account)
        .filter(|previous| {
            previous.get("expires").and_then(Value::as_u64).unwrap_or(0)
                >= source.get("expires").and_then(Value::as_u64).unwrap_or(0)
        })
        .cloned()
        .unwrap_or(source);
    let expires = previous.get("expires").and_then(Value::as_u64).unwrap_or(0);
    let value =
        if expires > 0 && expires <= now() + 60_000 && !text(&previous, "refresh").is_empty() {
            renew(provider, client, &previous).await?
        } else {
            previous
        };
    if held.len() >= 128 && !held.contains_key(&source_account) {
        return Err(invalid("Too many desktop source accounts are active."));
    }
    held.insert(source_account, value.clone());
    Ok(value)
}

pub(super) fn domain(site: Site, credentials: &Value) -> String {
    let domain = text(credentials, "domain");
    if domain.is_empty() {
        url::Url::parse(site.endpoint())
            .unwrap()
            .host_str()
            .unwrap()
            .into()
    } else {
        domain.into()
    }
}
pub(super) fn enterprise_headers(credentials: &Value) -> BTreeMap<String, String> {
    let enterprise = text(credentials, "enterpriseId").trim();
    if enterprise.is_empty() {
        BTreeMap::new()
    } else {
        BTreeMap::from([
            ("x-enterprise-id".into(), enterprise.into()),
            ("x-tenant-id".into(), enterprise.into()),
        ])
    }
}
fn default_source(site: Site) -> PluginResult<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| invalid("The home directory is unavailable."))?;
    let root = PathBuf::from(home);
    let base = if cfg!(target_os = "macos") {
        root.join("Library/Application Support/CodeBuddyExtension")
    } else if cfg!(target_os = "windows") {
        root.join("AppData/Local/CodeBuddyExtension")
    } else {
        root.join(".local/share/CodeBuddyExtension")
    };
    Ok(base
        .join("Data/Public/auth")
        .join(format!("{}.info", site.source_id())))
}
pub(super) fn read_source(site: Site, marker: &Value) -> PluginResult<Value> {
    let path = match marker.get("sourcePath").and_then(Value::as_str) {
        Some(path) => PathBuf::from(path),
        None => default_source(site)?,
    };
    let value = read_file(&path)?;
    if text(marker, "sourceUID").is_empty() || text(&value, "uid") != text(marker, "sourceUID") {
        return Err(invalid(
            "The WorkBuddy desktop account changed. Select the desktop source again.",
        ));
    }
    if marker.get("enterpriseId").is_some()
        && text(marker, "enterpriseId") != text(&value, "enterpriseId")
    {
        return Err(invalid(
            "The WorkBuddy desktop billing account changed. Reconnect the account.",
        ));
    }
    Ok(value)
}
pub(super) fn discover(site: Site) -> PluginResult<Value> {
    discover_at(site, &default_source(site)?)
}

fn discover_at(site: Site, path: &Path) -> PluginResult<Value> {
    let Some(account) = read_optional_file(path)? else {
        return Ok(json!({"accounts":[]}));
    };
    let credentials = source_credentials(site, path, &account)?;
    Ok(json!({"accounts":[{"sourceID":"desktop","credentials":credentials}]}))
}

fn source_credentials(site: Site, path: &Path, account: &Value) -> PluginResult<Value> {
    let uid = text(account, "uid");
    let mut saved = json!({
        "type":"oauth", "authMethod":"desktop", "source":"desktop",
        "sourcePath":path, "sourceUID":uid, "uid":uid,
        "enterpriseId":text(account, "enterpriseId"), "name":account.get("name"), "accountId":uid,
        "extras":{"noraAccountSource":{"kind":"desktop","path":path}}
    });
    assign_identity(site, &mut saved)?;
    Ok(saved)
}

pub(super) fn assign_identity(site: Site, credentials: &mut Value) -> PluginResult<()> {
    let subject = text(credentials, "uid");
    let scope = text(credentials, "enterpriseId");
    if subject.is_empty()
        || subject.len() > 512
        || subject.chars().any(char::is_control)
        || scope.len() > 512
        || scope.chars().any(char::is_control)
    {
        return Err(invalid(
            "WorkBuddy did not return a stable account identity.",
        ));
    }
    let mut identity = json!({"issuer":site.endpoint(),"subject":subject});
    if !scope.is_empty() {
        identity["scope"] = json!(scope);
    }
    credentials["accountIdentity"] = identity.clone();
    if !credentials["extras"].is_object() {
        credentials["extras"] = json!({});
    }
    credentials["extras"]["accountIdentity"] = identity;
    Ok(())
}

pub(super) fn read_file(path: &Path) -> PluginResult<Value> {
    read_optional_file(path)?.ok_or_else(signed_out)
}

fn read_optional_file(path: &Path) -> PluginResult<Option<Value>> {
    if !path.is_absolute() {
        return Err(invalid("Choose an absolute desktop account path."));
    }
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(invalid("The WorkBuddy desktop sign-in cannot be read.")),
    };
    let metadata = file
        .metadata()
        .map_err(|_| invalid("The desktop account cannot be read."))?;
    if !metadata.is_file() || metadata.len() > 1024 * 1024 {
        return Err(invalid("The desktop account file is invalid or too large."));
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid("The desktop account cannot be read."))?;
    if bytes.len() > 1024 * 1024 {
        return Err(invalid("The desktop account file is too large."));
    }
    let source: Value = serde_json::from_slice(&bytes)
        .map_err(|_| invalid("The desktop account file is invalid."))?;
    let Some(token) = source.get("auth").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    if !token.is_object() {
        return Err(invalid("The WorkBuddy desktop sign-in format is invalid."));
    }
    if token.pointer("/accessToken/$wbEncrypted").is_some()
        || token.pointer("/refreshToken/$wbEncrypted").is_some()
    {
        return Err(invalid(
            "WorkBuddy desktop encrypts this sign-in. Use browser sign-in instead.",
        ));
    }
    if token
        .get("accessToken")
        .is_none_or(|value| value.is_null() || value.as_str() == Some(""))
    {
        return Ok(None);
    }
    let mut account = merge(&json!({"type":"oauth"}), token)?;
    let Some(who) = source.get("account").filter(|value| value.is_object()) else {
        return Err(invalid(
            "The WorkBuddy desktop account identity cannot be read.",
        ));
    };
    if text(who, "uid").is_empty() {
        return Err(invalid(
            "The WorkBuddy desktop account identity cannot be read.",
        ));
    }
    for field in ["uid", "enterpriseId"] {
        if !text(who, field).is_empty() {
            account[field] = who[field].clone();
        }
    }
    account["name"] = json!(["nickname", "phoneNumber", "uid"]
        .into_iter()
        .map(|field| text(who, field))
        .find(|value| !value.is_empty())
        .unwrap_or(""));
    Ok(Some(account))
}

#[cfg(test)]
#[path = "../test/account_discovery.test.rs"]
mod account_discovery_tests;
