use super::*;
use base64::Engine;

#[derive(Default)]
pub(super) struct Entry {
    pub email: String,
    pub access: String,
    pub refresh: String,
    pub project: String,
    pub error: Option<String>,
}
impl Entry {
    pub fn secrets(&self) -> Vec<String> {
        vec![self.access.clone(), self.refresh.clone()]
    }
}

pub(super) fn parse(app: App, input: &ImportInput<'_>) -> PluginResult<Vec<Entry>> {
    if !FORMATS.contains(&input.format) {
        return Err(shared::invalid(
            "This provider does not support that account import format.",
        ));
    }
    let content = input.content.trim_start_matches('\u{feff}').trim();
    let mut entries = Vec::new();
    if content.starts_with(['{', '[']) {
        let value: Value = serde_json::from_str(content)
            .map_err(|_| shared::invalid("The account file is not valid JSON."))?;
        if !shape_matches(&value, input.format) {
            return Err(shared::invalid(
                "The account content does not match the selected import format.",
            ));
        }
        walk(app, &value, 0, &mut entries)?;
    } else {
        if !matches!(input.format, "auto" | "refresh-token") {
            return Err(shared::invalid(
                "The selected format requires a JSON account file.",
            ));
        }
        for line in content
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
        {
            entries.push(entry(app, &json!({"refresh_token":line})));
        }
    }
    if entries.is_empty() {
        return Err(shared::invalid(
            "The file does not contain any account entries.",
        ));
    }
    if entries.len() > shared::MAX_ENTRIES {
        return Err(shared::invalid("Import at most 200 accounts at a time."));
    }
    Ok(entries)
}
fn walk(app: App, value: &Value, depth: usize, entries: &mut Vec<Entry>) -> PluginResult<()> {
    if depth > 32 || entries.len() > shared::MAX_ENTRIES {
        return Err(shared::invalid(
            "The account file contains too many entries or nested groups.",
        ));
    }
    match value {
        Value::Array(values) => {
            for value in values {
                walk(app, value, depth + 1, entries)?;
            }
        }
        Value::String(refresh) => entries.push(entry(app, &json!({"refresh_token":refresh}))),
        Value::Object(object) => {
            if object.get("accounts").is_some_and(Value::is_array)
                && read(value, &["refresh_token", "refreshToken", "refresh"]).is_empty()
            {
                walk(app, &object["accounts"], depth + 1, entries)?;
            } else {
                entries.push(entry(app, value));
            }
        }
        _ => entries.push(Entry {
            error: Some("This entry is not an account.".into()),
            ..Default::default()
        }),
    }
    Ok(())
}
fn entry(app: App, value: &Value) -> Entry {
    let mut outer = value.clone();
    if let Some(agent) = value.get("agent").and_then(Value::as_str) {
        if !(agent == app.id() || (app == App::GeminiCLI && agent == "gemini")) {
            return Entry {
                error: Some("This saved login belongs to another Google application.".into()),
                ..Default::default()
            };
        }
        outer = match value.get("auth") {
            Some(Value::String(auth)) => serde_json::from_str(auth).unwrap_or(Value::Null),
            Some(Value::Object(auth)) => Value::Object(auth.clone()),
            _ => Value::Null,
        };
        if !outer.is_object() {
            return Entry {
                error: Some("The saved login does not contain Google account credentials.".into()),
                ..Default::default()
            };
        }
        if outer.get("email").is_none() {
            outer["email"] = value.get("user").cloned().unwrap_or(Value::Null);
        }
    }
    let value = &outer;
    let nested = value
        .get("token")
        .filter(|value| value.is_object())
        .unwrap_or(value);
    let mut entry = Entry {
        email: first(&[read(value, &["email"]), read(nested, &["email"])]).into(),
        access: first(&[
            read(value, &["access_token", "accessToken", "access"]),
            read(nested, &["access_token", "accessToken", "access"]),
        ])
        .into(),
        refresh: first(&[
            read(value, &["refresh_token", "refreshToken", "refresh"]),
            read(nested, &["refresh_token", "refreshToken", "refresh"]),
        ])
        .into(),
        project: first(&[
            read(value, &["project_id", "projectId", "project"]),
            read(nested, &["project_id", "projectId", "project"]),
        ])
        .into(),
        error: None,
    };
    let typ = read(value, &["type"]).to_ascii_lowercase();
    let application = read(value, &["app"]);
    if !typ.is_empty()
        && typ != app.id()
        && !(app == App::GeminiCLI && typ == "gemini")
        && typ != "authorized_user"
        && !(typ == "oauth" && application == app.id())
    {
        entry.error =
            Some("This account belongs to another Google application or is an API key.".into());
    }
    for value in [value, nested] {
        for client in ["client_id", "clientId"] {
            if value
                .get(client)
                .is_some_and(|value| value.as_str() != Some(app.client_id()))
            {
                entry.error = Some("This token belongs to another Google OAuth client.".into());
            }
        }
        if let Some(claims) = value
            .get("id_token")
            .or_else(|| value.get("idToken"))
            .and_then(Value::as_str)
            .and_then(jwt_claims)
        {
            if claims.get("aud").is_some_and(|aud| match aud {
                Value::String(aud) => aud != app.client_id(),
                Value::Array(auds) => !auds.iter().any(|aud| aud.as_str() == Some(app.client_id())),
                _ => true,
            }) {
                entry.error = Some("This token belongs to another Google OAuth client.".into());
            }
        }
    }
    if value.get("source").is_some() || value.pointer("/extras/noraAccountSource").is_some() {
        entry.error=Some("A local sign-in reference contains no transferable credentials. Export the account's credentials instead.".into());
    }
    if entry.refresh.is_empty() {
        entry.error.get_or_insert_with(|| {
            "The account has no refresh token and cannot be renewed.".into()
        });
    }
    if [entry.refresh.as_str(), entry.access.as_str()]
        .iter()
        .any(|token| {
            token.len() > 64 * 1024
                || token.chars().any(char::is_whitespace)
                || token.chars().any(char::is_control)
        })
    {
        entry.error = Some("The account token format is invalid.".into());
    }
    if entry.email.len() > 512
        || (!entry.email.is_empty()
            && (!entry.email.contains('@')
                || entry.email.chars().any(char::is_whitespace)
                || entry.email.chars().any(char::is_control)))
    {
        entry.email.clear();
    }
    if entry.project.len() > 512
        || entry.project.chars().any(char::is_whitespace)
        || entry.project.chars().any(char::is_control)
    {
        entry.error = Some("The Google Cloud project format is invalid.".into());
    }
    entry
}
fn read<'a>(value: &'a Value, keys: &[&str]) -> &'a str {
    keys.iter()
        .find_map(|key| {
            value
                .get(*key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_default()
}
fn first<'a>(values: &[&'a str]) -> &'a str {
    values
        .iter()
        .copied()
        .find(|value| !value.is_empty())
        .unwrap_or_default()
}
fn jwt_claims(token: &str) -> Option<Value> {
    let part = token.split('.').nth(1)?;
    if part.len() > 128 * 1024 {
        return None;
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(part)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(part))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}
fn shape_matches(value: &Value, format: &str) -> bool {
    if format == "auto" {
        return true;
    }
    if let Some(values) = value.as_array() {
        return values.iter().all(|value| shape_matches(value, format));
    }
    match format {
        "google-account" => {
            value.is_object()
                && (value.get("refresh_token").is_some()
                    || value.get("refreshToken").is_some()
                    || value.get("accounts").is_some_and(Value::is_array))
        }
        "antigravity-manager" => {
            value.get("token").is_some_and(Value::is_object)
                || value.get("accounts").is_some_and(Value::is_array)
                || value.get("refresh_token").is_some()
        }
        "cli-proxy" => value.get("type").is_some_and(Value::is_string),
        "nora-account" => value["type"] == "oauth" && value.get("app").is_some(),
        "magpie-login" => {
            value.get("agent").is_some_and(Value::is_string) && value.get("auth").is_some()
        }
        "refresh-token" => value.is_string(),
        _ => false,
    }
}
