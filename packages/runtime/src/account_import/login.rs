use super::{parse::Entry, *};
use base64::Engine;
use nora_ai::{HttpClient, HttpMethod, HttpRequest};
use nora_provider_auth::OpenAICodexOAuthProvider;
use std::time::Duration;

pub(super) async fn import(
    kind: LoginKind,
    entry: &Entry,
    resumed: Option<Value>,
    fingerprint: &str,
    client: &dyn HttpClient,
    persistence: &dyn CredentialPersistence,
) -> PluginResult<Value> {
    let mut credentials = match resumed {
        Some(credentials) => validate_resume(kind, credentials)?,
        None => initial(kind, entry, fingerprint),
    };
    checkpoint(persistence, &credentials).await?;
    if kind == LoginKind::Claude {
        return result(credentials, false);
    }
    let valid_access = token(credentials.get("access")).is_some()
        && credentials
            .get("expires")
            .and_then(Value::as_i64)
            .is_some_and(|expiry| expiry > nora_ai::oauth::now_millis().saturating_add(30_000));
    if !valid_access {
        credentials = refresh(credentials, fingerprint, client, persistence).await?;
    }
    finish_codex(&mut credentials, entry, fingerprint);
    mark(&mut credentials, fingerprint, "complete");
    checkpoint(persistence, &credentials).await?;
    result(credentials, true)
}

fn initial(kind: LoginKind, entry: &Entry, fingerprint: &str) -> Value {
    let mut credentials = json!({
        "type":"oauth", "authMethod":"oauth",
        "provider":if kind==LoginKind::Codex {"openAICodex"} else {"anthropic"},
        "credentialProvider":if kind==LoginKind::Codex {"openAICodex"} else {"anthropic"},
        "authProvider":kind.id(), "access":entry.access, "refresh":entry.refresh,
        "expires":1, "email":entry.email, "label":entry.email, "plan":entry.plan,
        "extras":{"email":entry.email,"plan":entry.plan}
    });
    if kind == LoginKind::Codex {
        if !entry.id_token.is_empty() {
            credentials["extras"]["idToken"] = json!(entry.id_token);
        }
    } else {
        let mut identity = if entry.account.is_empty() {
            json!({"issuer":"urn:nora:import:claude-unverified", "subject":entry.email.to_lowercase()})
        } else {
            json!({"issuer":"https://claude.ai", "subject":entry.account})
        };
        if !entry.organization.is_empty() {
            identity["scope"] = json!(entry.organization);
        }
        let scopes = if entry.scopes.is_empty() {
            vec![
                "user:inference",
                "user:profile",
                "user:sessions:claude_code",
                "user:mcp_servers",
            ]
            .into_iter()
            .map(str::to_string)
            .collect()
        } else {
            entry.scopes.clone()
        };
        let identity_key = if entry.account.is_empty() {
            entry.email.to_lowercase()
        } else {
            entry.account.clone()
        };
        credentials["accountId"] = json!(super::fingerprint(
            "claude-account",
            &format!("{identity_key}\0{}", entry.organization)
        ));
        credentials["accountIdentity"] = identity.clone();
        credentials["extras"]["accountIdentity"] = identity;
        credentials["extras"]["scopes"] = json!(scopes);
    }
    mark(
        &mut credentials,
        fingerprint,
        if kind == LoginKind::Claude {
            "complete"
        } else {
            "pending"
        },
    );
    credentials
}

fn validate_resume(kind: LoginKind, mut credentials: Value) -> PluginResult<Value> {
    let provider = credentials
        .get("credentialProvider")
        .or_else(|| credentials.get("provider"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let provider_matches = match kind {
        LoginKind::Codex => matches!(provider, "openAICodex" | "openai-codex"),
        LoginKind::Claude => provider == "anthropic",
    };
    if credentials["type"] != "oauth"
        || !provider_matches
        || token(credentials.get("refresh")).is_none()
        || (kind == LoginKind::Claude && token(credentials.get("access")).is_none())
    {
        return Err(invalid("The saved import does not match this provider."));
    }
    // A private staging record may only restore transferable token values.
    for key in ["key", "source", "noraClaudeCLI"] {
        credentials.as_object_mut().unwrap().remove(key);
    }
    if let Some(extras) = credentials.get_mut("extras").and_then(Value::as_object_mut) {
        extras.remove("noraClaudeCLI");
        extras.remove("noraAccountSource");
    }
    Ok(credentials)
}

async fn refresh(
    mut credentials: Value,
    fingerprint: &str,
    client: &dyn HttpClient,
    persistence: &dyn CredentialPersistence,
) -> PluginResult<Value> {
    let provider = OpenAICodexOAuthProvider::default();
    let refresh = token(credentials.get("refresh"))
        .ok_or_else(|| invalid("The account has no refresh token."))?;
    let form = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs([
            ("client_id", provider.client_id.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
        ])
        .finish();
    let payload = tokio::time::timeout(Duration::from_secs(35), async {
        let response = client
            .stream(
                HttpRequest::new(HttpMethod::Post, provider.token_url)
                    .with_header("content-type", "application/x-www-form-urlencoded")
                    .with_header("accept", "application/json")
                    .with_body(form),
            )
            .await
            .map_err(|_| renewal_error())?;
        if !(200..300).contains(&response.status) {
            return Err(renewal_error());
        }
        let body = nora_ai::collect_http_body_text_limited(response.body, MAX_BYTES + 1)
            .await
            .map_err(|_| renewal_error())?;
        if body.len() > MAX_BYTES {
            return Err(renewal_error());
        }
        serde_json::from_str::<Value>(&body).map_err(|_| renewal_error())
    })
    .await
    .map_err(|_| renewal_error())??;

    // A server can rotate a refresh token even when the remaining response is
    // incomplete. Keep that new token before validation or identity work.
    let rotated = token(payload.get("refresh_token")).map(str::to_owned);
    if let Some(refresh) = rotated.as_ref() {
        credentials["refresh"] = json!(refresh);
    }
    if let Some(access) = token(payload.get("access_token")) {
        credentials["access"] = json!(access);
    }
    let expires = expiry(&payload);
    credentials["expires"] = json!(expires.unwrap_or(0));
    // Only freshly issued claims can identify the account. Imported JWTs remain
    // display data and must not decide which existing account gets replaced.
    credentials["extras"]
        .as_object_mut()
        .unwrap()
        .remove("idToken");
    if let Some(id) = token(payload.get("id_token")) {
        credentials["extras"]["idToken"] = json!(id);
    }
    mark(&mut credentials, fingerprint, "rotated");
    checkpoint(persistence, &credentials).await?;
    if token(payload.get("access_token")).is_none()
        || expires.is_none()
        || (payload.get("refresh_token").is_some() && rotated.is_none())
    {
        return Err(renewal_error());
    }
    Ok(credentials)
}

fn finish_codex(credentials: &mut Value, entry: &Entry, fingerprint: &str) {
    let access = credentials["access"].as_str().unwrap_or_default();
    let id = credentials
        .pointer("/extras/idToken")
        .and_then(Value::as_str);
    let metadata = OpenAICodexOAuthProvider::profile_extras_from_tokens(access, id);
    let identity=metadata.get("accountIdentity").map(|value|value.as_value().clone())
        .or_else(||{
            let email=metadata.get("email")?.as_str()?;
            let account=OpenAICodexOAuthProvider::extract_account_id_from_jwt(access)
                .or_else(||id.and_then(OpenAICodexOAuthProvider::extract_account_id_from_jwt))?;
            Some(json!({"issuer":"urn:nora:import:openai-email","subject":email.to_lowercase(),"scope":account}))
        }).unwrap_or_else(||json!({"issuer":"urn:nora:import:openai-unverified","subject":fingerprint}));
    let secrets = [
        entry.secrets(),
        vec![
            credentials["access"].as_str().unwrap_or_default().into(),
            credentials["refresh"].as_str().unwrap_or_default().into(),
            id.unwrap_or_default().into(),
        ],
    ]
    .concat();
    for key in ["email", "name", "plan"] {
        let fallback = match key {
            "email" => entry.email.as_str(),
            "plan" => entry.plan.as_str(),
            _ => "",
        };
        let value = redact(
            metadata
                .get(key)
                .and_then(|value| value.as_str())
                .unwrap_or(fallback)
                .to_string(),
            &secrets,
        );
        credentials[key] = json!(value);
        credentials["extras"][key] = credentials[key].clone();
    }
    credentials["label"] = credentials["email"].clone();
    credentials["accountId"] = identity
        .get("scope")
        .cloned()
        .unwrap_or_else(|| json!(fingerprint));
    credentials["accountIdentity"] = identity.clone();
    credentials["extras"]["accountIdentity"] = identity;
}
fn expiry(payload: &Value) -> Option<i64> {
    let now = nora_ai::oauth::now_millis();
    if let Some(seconds) = payload.get("expires_in") {
        return seconds
            .as_i64()
            .filter(|seconds| *seconds > 0 && *seconds <= 31_536_000)
            .map(|seconds| now.saturating_add(seconds.saturating_mul(1000)));
    }
    let access = token(payload.get("access_token"))?;
    let part = access.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(part)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(part))
        .ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    claims
        .get("exp")?
        .as_i64()?
        .checked_mul(1000)
        .filter(|expiry| *expiry > now && *expiry <= now.saturating_add(31_536_000_000))
}

pub(super) fn token(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).filter(|value| {
        !value.is_empty()
            && value.len() <= 256 * 1024
            && !value.chars().any(char::is_whitespace)
            && !value.chars().any(char::is_control)
    })
}
pub async fn checkpoint(
    persistence: &dyn CredentialPersistence,
    credentials: &Value,
) -> PluginResult<()> {
    persistence.save(credentials.clone()).await.map_err(|_|PluginError::new("credential_persistence","The imported credentials could not be saved. Retry this entry before using the account."))
}
fn renewal_error() -> PluginError {
    PluginError::authentication("The imported ChatGPT account could not be renewed. Export fresh account credentials or sign in directly.")
}
