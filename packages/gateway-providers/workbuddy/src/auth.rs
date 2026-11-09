use super::*;
use nora_plugin_runtime_sdk::provider::{Account, Step};
use sha2::{Digest, Sha256};

/// A browser sign-in waiting on its user: the state WorkBuddy polls by,
/// and the token once the user approved.
pub(super) struct Pending {
    state: String,
    deadline: u64,
    token: Option<Value>,
}

/// Starts a browser sign-in: WorkBuddy's page, opened by the user, and the
/// session the router asks to finish with.
pub(super) async fn sign_in(provider: &WorkBuddy, client: &dyn HttpClient) -> PluginResult<Step> {
    let data = provider
        .call(
            client,
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
            token: None,
        },
    );
    Ok(Step::Open {
        url: url.to_string(),
        code: None,
        session,
    })
}

/// Asks WorkBuddy once whether the user finished: the token first, then the
/// account it belongs to. Either not ready yet is waiting.
pub(super) async fn finish(
    provider: &WorkBuddy,
    client: &dyn HttpClient,
    session: &str,
) -> PluginResult<Step> {
    let mut sessions = provider.pending.lock().await;
    let pending = sessions
        .get_mut(session)
        .filter(|pending| pending.deadline > now())
        .ok_or_else(|| invalid("The WorkBuddy sign-in expired. Start it again."))?;
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("state", &pending.state)
        .finish();
    let token = match &pending.token {
        Some(token) => token.clone(),
        None => {
            let path = format!("/v2/plugin/auth/token?{query}");
            let Some(token) = ask(provider, client, &path, BTreeMap::new(), 11217).await? else {
                return Ok(Step::Waiting);
            };
            pending.token = Some(token.clone());
            token
        }
    };
    let mut credentials = merge(&json!({"type":"oauth"}), &token)?;
    let who = ask(
        provider,
        client,
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
    let Some(who) = who else {
        return Ok(Step::Waiting);
    };
    sessions.remove(session);
    drop(sessions);
    let uid = text(&who, "uid");
    if uid.is_empty() || uid.len() > 1024 || uid.chars().any(char::is_control) {
        return Err(invalid("WorkBuddy returned no account identity."));
    }
    let enterprise = enterprise(provider, client, &credentials, &who).await?;
    credentials["uid"] = json!(uid);
    credentials["accountId"] = json!(uid);
    let name = ["nickname", "phoneNumber", "uid"]
        .into_iter()
        .map(|key| text(&who, key))
        .find(|text| !text.is_empty())
        .unwrap_or(uid)
        .to_owned();
    credentials["name"] = json!(name);
    if !enterprise.is_empty() {
        credentials["enterpriseId"] = json!(enterprise);
    }
    assign_identity(provider.site, &mut credentials)?;
    Ok(Step::Account(Account {
        name: account_name(&name, &enterprise),
        expires_at: expires_at(&credentials),
        credentials,
    }))
}

/// One look at a sign-in endpoint: its data once there is some, none while
/// WorkBuddy still says to wait (`retry`, a timeout or too many asks).
async fn ask(
    provider: &WorkBuddy,
    client: &dyn HttpClient,
    path: &str,
    headers: BTreeMap<String, String>,
    retry: i64,
) -> PluginResult<Option<Value>> {
    match provider
        .call(client, HttpMethod::Get, path, headers, None)
        .await
    {
        Ok(value) if value.as_object().is_some_and(|object| !object.is_empty()) => Ok(Some(value)),
        Ok(_) => Ok(None),
        Err(error)
            if error.code == format!("workbuddy_{retry}")
                || error.message.contains("HTTP 408,")
                || error.message.contains("HTTP 429,") =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// How an account shows: its name, with its enterprise when it has one, so
/// a personal and a team account of one person are told apart.
fn account_name(name: &str, enterprise: &str) -> String {
    if enterprise.is_empty() {
        name.into()
    } else {
        format!("{name} ({enterprise})")
    }
}

/// When the credentials' access token expires, for the router to renew it
/// before then.
pub(super) fn expires_at(credentials: &Value) -> Option<String> {
    credentials
        .get("expires")
        .and_then(Value::as_u64)
        .filter(|expires| *expires > 0)
        .map(|expires| {
            nora_plugin_runtime_sdk::provider::rfc3339(
                std::time::UNIX_EPOCH + Duration::from_millis(expires),
            )
        })
}

/// The credentials a request goes with: the router renews them before they
/// expire, so they are used as handed over.
pub(super) fn current(credentials: &Value) -> PluginResult<Value> {
    if text(credentials, "access").is_empty() {
        return Err(signed_out());
    }
    Ok(credentials.clone())
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
