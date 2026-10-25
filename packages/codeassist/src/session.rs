use super::*;
use base64::Engine;
use std::io::Read;
use std::path::Path;

#[derive(Clone, Default)]
pub(crate) struct Tokens {
    pub access: String,
    pub refresh: String,
    pub expires: i64,
    pub project: String,
    pub uid: String,
    pub email: String,
    pub name: String,
    pub version: String,
}

pub(crate) fn selected(path: &str, app: App) -> PluginResult<Tokens> {
    nora_provider_plugin_sdk::require_subscription_accounts()?;
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err(invalid(
            "Select an absolute path to one Google account JSON file.",
        ));
    }
    let file = std::fs::File::open(path).map_err(|_| expired())?;
    if !file.metadata().map_err(|_| expired())?.is_file() {
        return Err(expired());
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| expired())?;
    if bytes.len() > 1024 * 1024 {
        return Err(expired());
    }
    let payload: Value = serde_json::from_slice(&bytes).map_err(|_| expired())?;
    if !payload.is_object() {
        return Err(invalid(
            "Select a file containing exactly one Google account.",
        ));
    }
    if let Some(kind) = text(payload.get("type"), 64) {
        if kind != app.id()
            && !(app == App::GeminiCLI && kind == "gemini")
            && kind != "authorized_user"
        {
            return Err(invalid(
                "The selected file belongs to another Google application.",
            ));
        }
    }
    let nested = payload
        .get("token")
        .filter(|value| value.is_object())
        .unwrap_or(&payload);
    for client in [payload.get("client_id"), nested.get("client_id")]
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if client != app.client_id() {
            return Err(invalid(
                "The selected token belongs to another Google OAuth client.",
            ));
        }
    }
    if let Some(jwt) = text(nested.get("id_token"), 64 * 1024) {
        if let Some(claims) = jwt
            .split('.')
            .nth(1)
            .and_then(|part| {
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(part)
                    .ok()
            })
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        {
            if claims
                .get("aud")
                .is_some_and(|aud| aud.as_str() != Some(app.client_id()))
            {
                return Err(invalid(
                    "The selected token belongs to another Google OAuth client.",
                ));
            }
        }
    }
    let refresh = token(
        nested
            .get("refresh_token")
            .or_else(|| nested.get("refreshToken")),
    )?;
    let access = text(
        nested
            .get("access_token")
            .or_else(|| nested.get("accessToken")),
        64 * 1024,
    )
    .unwrap_or_default()
    .into();
    let expires = nested
        .get("expiry_date")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let project = text(
        payload
            .get("project")
            .or_else(|| payload.get("project_id"))
            .or_else(|| nested.get("project_id")),
        512,
    )
    .unwrap_or_default()
    .into();
    Ok(Tokens {
        access,
        refresh,
        expires,
        project,
        ..Default::default()
    })
}

pub(crate) fn current(value: Option<&Value>, app: App) -> PluginResult<(Tokens, Option<String>)> {
    nora_provider_plugin_sdk::require_subscription_accounts()?;
    let value = value.ok_or_else(expired)?;
    if let Some(source) = value.get("source") {
        if source.get("app").and_then(Value::as_str) != Some(app.id()) {
            return Err(expired());
        }
        let path = text(source.get("path"), 16 * 1024).ok_or_else(expired)?;
        let uid = text(source.get("uid"), 128).ok_or_else(expired)?.to_owned();
        let mut tokens = selected(path, app)?;
        if let Some(project) = text(value.get("project"), 512) {
            tokens.project = project.into();
        }
        return Ok((tokens, Some(uid)));
    }
    if value.get("app").and_then(Value::as_str) != Some(app.id()) {
        return Err(expired());
    }
    Ok((
        Tokens {
            access: token(value.get("access"))?,
            refresh: token(value.get("refresh"))?,
            expires: value.get("expires").and_then(Value::as_i64).unwrap_or(0),
            project: text(value.get("project"), 512).unwrap_or_default().into(),
            uid: text(value.get("accountId"), 128).unwrap_or_default().into(),
            email: text(value.get("email"), 512).unwrap_or_default().into(),
            name: text(value.get("name"), 512).unwrap_or_default().into(),
            version: text(value.get("clientVersion"), 64)
                .unwrap_or("2.9.1")
                .into(),
        },
        None,
    ))
}

pub(crate) fn saved(tokens: &Tokens, app: App, path: Option<&str>) -> Value {
    if let Some(path) = path {
        return json!({"type":"codeAssistSource","authMethod":"source","app":app.id(),"label":tokens.email,"email":tokens.email,"name":tokens.name,"accountId":tokens.uid,"project":tokens.project,"source":{"app":app.id(),"path":path,"uid":tokens.uid}});
    }
    json!({"type":"oauth","authMethod":"oauth","app":app.id(),"label":tokens.email,"email":tokens.email,"name":tokens.name,"accountId":tokens.uid,"access":tokens.access,"refresh":tokens.refresh,"expires":tokens.expires,"project":tokens.project,"clientVersion":tokens.version})
}

#[cfg(test)]
#[path = "../test/session.test.rs"]
mod tests;
