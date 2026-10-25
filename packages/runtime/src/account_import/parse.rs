use super::*;

#[derive(Default)]
pub(super) struct Entry {
    pub email: String,
    pub access: String,
    pub refresh: String,
    pub id_token: String,
    pub account: String,
    pub organization: String,
    pub plan: String,
    pub scopes: Vec<String>,
    pub error: Option<String>,
}
impl Entry {
    pub fn secrets(&self) -> Vec<String> {
        vec![
            self.access.clone(),
            self.refresh.clone(),
            self.id_token.clone(),
        ]
    }
}
pub(super) fn parse(kind: LoginKind, input: &ImportInput<'_>) -> PluginResult<Vec<Entry>> {
    if !kind.formats().contains(&input.format) {
        return Err(invalid(
            "This provider does not support that account import format.",
        ));
    }
    let content = input.content.trim_start_matches('\u{feff}').trim();
    let mut entries = Vec::new();
    if content.starts_with(['{', '[']) {
        let value: Value = serde_json::from_str(content)
            .map_err(|_| invalid("The account file is not valid JSON."))?;
        if !shape_matches(kind, &value, input.format) {
            return Err(invalid(
                "The account content does not match the selected import format.",
            ));
        }
        walk(kind, &value, 0, &mut entries)?;
    } else {
        if !matches!(input.format, "auto" | "refresh-token") {
            return Err(invalid("The selected format requires a JSON account file."));
        }
        for line in content
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
        {
            entries.push(if line.chars().any(char::is_whitespace) {
                Entry {
                    error: Some("A refresh token must occupy a single line without spaces.".into()),
                    ..Default::default()
                }
            } else {
                entry(kind, &json!({"refresh_token":line}))
            });
        }
    }
    if entries.is_empty() {
        return Err(invalid("The file does not contain any account entries."));
    }
    if entries.len() > MAX_ENTRIES {
        return Err(invalid("Import at most 200 accounts at a time."));
    }
    Ok(entries)
}
fn walk(
    kind: LoginKind,
    value: &Value,
    depth: usize,
    entries: &mut Vec<Entry>,
) -> PluginResult<()> {
    if depth > 32 || entries.len() > MAX_ENTRIES {
        return Err(invalid(
            "The account file contains too many entries or nested groups.",
        ));
    }
    match value {
        Value::Array(values) => {
            for value in values {
                walk(kind, value, depth + 1, entries)?
            }
        }
        Value::String(refresh) => entries.push(entry(kind, &json!({"refresh_token":refresh}))),
        Value::Object(object) => {
            if object.get("providers").is_some_and(Value::is_array)
                || value
                    .pointer("/openAI/remoteConnectionAccounts")
                    .is_some_and(Value::is_array)
            {
                let mut found = Vec::new();
                if let Some(providers) = object.get("providers").and_then(Value::as_array) {
                    for provider in providers {
                        if provider["kind"] == "openai_oauth" {
                            if let Some(accounts) = provider["accounts"].as_array() {
                                found.extend(accounts.iter());
                            }
                        }
                    }
                }
                if let Some(accounts) = value
                    .pointer("/openAI/remoteConnectionAccounts")
                    .and_then(Value::as_array)
                {
                    found.extend(accounts.iter());
                }
                let mut seen = std::collections::BTreeSet::new();
                for account in found {
                    let refresh = text(account, &["refreshToken"]);
                    if !refresh.is_empty() && !seen.insert(refresh.to_string()) {
                        continue;
                    }
                    let mut account = account.clone();
                    account["type"] = json!("codex");
                    account["account_id"] = account["openAIAccountId"].clone();
                    if account["kind"] == "api_key" {
                        account["type"] = json!("apiKey");
                    }
                    entries.push(entry(kind, &account));
                }
            } else if object.get("accounts").is_some_and(Value::is_array)
                && text(value, &["refresh_token", "refreshToken", "refresh"]).is_empty()
            {
                walk(kind, &object["accounts"], depth + 1, entries)?;
            } else {
                entries.push(entry(kind, value));
            }
        }
        _ => entries.push(Entry {
            error: Some("This entry is not an account.".into()),
            ..Default::default()
        }),
    }
    Ok(())
}
fn entry(kind: LoginKind, value: &Value) -> Entry {
    let mut outer = value.clone();
    if let Some(auth) = value.get("auth") {
        if let Some(agent) = value.get("agent").and_then(Value::as_str) {
            outer = match auth {
                Value::String(auth) => serde_json::from_str(auth).unwrap_or(Value::Null),
                Value::Object(_) => auth.clone(),
                _ => Value::Null,
            };
            if !outer.is_object() {
                return Entry {
                    error: Some("The saved login does not contain account credentials.".into()),
                    ..Default::default()
                };
            }
            if !((kind == LoginKind::Codex && agent == "codex")
                || (kind == LoginKind::Claude && agent == "claude"))
            {
                return Entry {
                    error: Some("This account belongs to another provider.".into()),
                    ..Default::default()
                };
            }
            if outer.get("email").is_none() {
                outer["email"] = value.get("user").cloned().unwrap_or(Value::Null);
            }
            if outer.get("plan").is_none() {
                outer["plan"] = value.get("plan").cloned().unwrap_or(Value::Null);
            }
            if let Some(profile) = value
                .get("profile")
                .and_then(Value::as_str)
                .and_then(|profile| serde_json::from_str::<Value>(profile).ok())
            {
                outer["email"] = profile
                    .get("emailAddress")
                    .cloned()
                    .unwrap_or_else(|| outer["email"].clone());
                outer["account_uuid"] = profile["accountUuid"].clone();
                outer["org_uuid"] = profile["organizationUuid"].clone();
            }
        }
    }
    let value = &outer;
    let sub2 = value.get("credentials").filter(|value| value.is_object());
    let mut source = value;
    if let Some(nested) = sub2 {
        source = nested;
    } else if kind == LoginKind::Codex {
        source = value
            .get("tokens")
            .filter(|value| value.is_object())
            .unwrap_or(value);
    } else if kind == LoginKind::Claude {
        source = value
            .get("claudeAiOauth")
            .filter(|value| value.is_object())
            .unwrap_or(value);
    }
    let extra = value
        .get("extra")
        .or_else(|| value.get("extras"))
        .unwrap_or(&Value::Null);
    let mut entry = Entry {
        email: select(&[
            text(value, &["email", "email_address"]),
            text(source, &["email", "email_address"]),
            text(extra, &["email", "email_address"]),
        ])
        .into(),
        access: text(source, &["access_token", "accessToken", "access"]).into(),
        refresh: text(source, &["refresh_token", "refreshToken", "refresh"]).into(),
        id_token: select(&[
            text(source, &["id_token", "idToken"]),
            text(extra, &["id_token", "idToken"]),
        ])
        .into(),
        account: select(&[
            text(
                source,
                &[
                    "chatgpt_account_id",
                    "account_id",
                    "accountId",
                    "account_uuid",
                ],
            ),
            text(extra, &["account_uuid"]),
            text(value, &["account_uuid"]),
        ])
        .into(),
        organization: select(&[
            text(
                source,
                &["org_uuid", "organization_uuid", "organizationUuid"],
            ),
            text(
                extra,
                &["org_uuid", "organization_uuid", "organizationUuid"],
            ),
            text(
                value,
                &["org_uuid", "organization_uuid", "organizationUuid"],
            ),
        ])
        .into(),
        plan: select(&[
            text(source, &["subscriptionType", "plan"]),
            text(value, &["plan"]),
            text(extra, &["plan"]),
        ])
        .into(),
        scopes: scopes(
            source
                .get("scopes")
                .or_else(|| source.get("scope"))
                .or_else(|| extra.get("scopes")),
        ),
        error: None,
    };
    if kind == LoginKind::Claude {
        entry.account = select(&[
            text(source, &["account_uuid", "accountUuid"]),
            text(extra, &["account_uuid", "accountUuid"]),
            text(value, &["account_uuid", "accountUuid"]),
        ])
        .into();
        if let Some(identity) = value
            .get("accountIdentity")
            .or_else(|| value.pointer("/extras/accountIdentity"))
            .filter(|identity| identity["issuer"] == "https://claude.ai")
        {
            entry.account = text(identity, &["subject"]).into();
            entry.organization = text(identity, &["scope"]).into();
        }
    }
    if entry.email.is_empty() && text(value, &["name"]).contains('@') {
        entry.email = text(value, &["name"]).into();
    }
    let typ = text(value, &["type"]).to_ascii_lowercase();
    let ours = match kind {
        LoginKind::Codex => &["codex", "openai", "openaicodex", "openai-codex"][..],
        LoginKind::Claude => &["claude", "anthropic"][..],
    };
    let provider = text(value, &["credentialProvider", "authProvider", "provider"]);
    if sub2.is_some() {
        let platform = text(value, &["platform"]).to_ascii_lowercase();
        if !ours.contains(&platform.as_str()) || typ != "oauth" {
            entry.error = Some("This entry is not this provider's OAuth account.".into());
        }
    } else if !typ.is_empty()
        && !ours.contains(&typ.as_str())
        && !(typ == "oauth"
            && ours
                .iter()
                .any(|expected| provider.eq_ignore_ascii_case(expected)))
    {
        entry.error = Some("This account belongs to another provider or is an API key.".into());
    }
    if (kind == LoginKind::Codex && value.get("claudeAiOauth").is_some())
        || (kind == LoginKind::Claude && value.get("tokens").is_some())
    {
        entry.error = Some("This account belongs to another provider.".into());
    }
    if value.get("source").is_some() || value.pointer("/extras/noraAccountSource").is_some() {
        entry.error=Some("A local sign-in reference contains no transferable credentials. Export the account's credentials instead.".into());
    }
    if entry.refresh.is_empty() {
        entry.error.get_or_insert_with(|| {
            if !text(value, &["OPENAI_API_KEY", "api_key", "apiKey", "key"]).is_empty() {
                "This is an API key. Add it as an API key instead.".into()
            } else {
                "The account has no refresh token and cannot be renewed.".into()
            }
        });
    }
    if [
        entry.refresh.as_str(),
        entry.access.as_str(),
        entry.id_token.as_str(),
    ]
    .iter()
    .any(|token| {
        token.len() > 256 * 1024
            || token.chars().any(char::is_control)
            || token.chars().any(char::is_whitespace)
    }) {
        entry.error = Some("The account token format is invalid.".into());
    }
    if entry.email.len() > 512
        || (!entry.email.is_empty()
            && (!entry.email.contains('@') || entry.email.chars().any(char::is_whitespace)))
    {
        entry.email.clear();
    }
    if [&entry.account, &entry.organization, &entry.plan]
        .iter()
        .any(|value| value.len() > 512 || value.chars().any(char::is_control))
        || entry.scopes.len() > 128
        || entry.scopes.iter().any(|scope| {
            scope.len() > 256
                || scope.chars().any(char::is_whitespace)
                || scope.chars().any(char::is_control)
        })
    {
        entry.error = Some("The account metadata format is invalid.".into());
    }
    if kind == LoginKind::Claude && entry.email.is_empty() {
        entry.error.get_or_insert_with(|| {
            "The file does not identify the Claude account. Sign in to it directly instead.".into()
        });
    }
    if kind == LoginKind::Claude && entry.access.is_empty() {
        entry.error.get_or_insert_with(|| {
            "The Claude export must include its access and refresh tokens.".into()
        });
    }
    entry
}
fn shape_matches(kind: LoginKind, value: &Value, format: &str) -> bool {
    if format == "auto" {
        return true;
    }
    if let Some(values) = value.as_array() {
        return values
            .iter()
            .all(|value| shape_matches(kind, value, format));
    }
    match format {
        "codex-auth" => value.get("tokens").is_some_and(Value::is_object),
        "claude-credentials" => value.get("claudeAiOauth").is_some_and(Value::is_object),
        "cli-proxy" => value.get("type").is_some_and(Value::is_string),
        "cockpit" => {
            value.is_object()
                && (value.get("tokens").is_some()
                    || value.get("refresh_token").is_some()
                    || value.get("refreshToken").is_some())
        }
        "sub2api" => {
            value.get("accounts").is_some_and(Value::is_array)
                || value.get("credentials").is_some_and(Value::is_object)
        }
        "codexbar" => {
            value.get("providers").is_some_and(Value::is_array)
                || value
                    .pointer("/openAI/remoteConnectionAccounts")
                    .is_some_and(Value::is_array)
        }
        "refresh-token" => value.is_string(),
        "nora-account" => value["type"] == "oauth" && value.get("credentialProvider").is_some(),
        "magpie-login" => {
            value.get("agent").is_some_and(Value::is_string) && value.get("auth").is_some()
        }
        _ => false,
    }
}
fn text<'a>(value: &'a Value, keys: &[&str]) -> &'a str {
    keys.iter()
        .find_map(|key| {
            value
                .get(*key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or("")
}
fn select<'a>(values: &[&'a str]) -> &'a str {
    values
        .iter()
        .copied()
        .find(|value| !value.is_empty())
        .unwrap_or("")
}
fn scopes(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(scopes)) => scopes.split_whitespace().map(str::to_string).collect(),
        Some(Value::Array(scopes)) => scopes
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}
