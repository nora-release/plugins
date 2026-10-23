use super::network;
use nora_ai::HttpClient;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::{io::Read, path::Path};
pub(crate) fn text(value: Option<&Value>, max: usize) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control))
}
pub(crate) fn expired() -> PluginError {
    PluginError::authentication("Devin sign-in expired or changed. Reconnect this account.")
}
pub(crate) fn server(raw: &str) -> PluginResult<String> {
    let url = url::Url::parse(raw)
        .map_err(|_| PluginError::invalid("The selected Devin API server is invalid."))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(PluginError::invalid(
            "Devin requires an HTTPS API origin without credentials or paths.",
        ));
    }
    Ok(url.as_str().trim_end_matches('/').into())
}
#[derive(Clone)]
pub(crate) struct Account {
    pub key: String,
    pub server: String,
    pub email: String,
    pub plan: String,
    pub source: Option<String>,
    pub saved: Value,
}
impl Account {
    pub fn parse(value: &Value) -> PluginResult<Self> {
        let source = text(value.pointer("/source/path"), 16 * 1024).map(str::to_owned);
        let key = if source.is_some() {
            String::new()
        } else {
            text(value.get("key").or_else(|| value.get("access")), 64 * 1024)
                .filter(|s| !s.chars().any(char::is_whitespace))
                .ok_or_else(expired)?
                .into()
        };
        Ok(Self {
            key,
            server: server(
                text(value.pointer("/metadata/server"), 2048).unwrap_or(network::SERVER),
            )?,
            email: text(
                value
                    .get("accountId")
                    .or_else(|| value.pointer("/metadata/email")),
                512,
            )
            .unwrap_or_default()
            .into(),
            plan: text(value.pointer("/metadata/plan"), 512)
                .unwrap_or_default()
                .into(),
            source,
            saved: value.clone(),
        })
    }
    pub async fn live(value: &Value, client: &dyn HttpClient) -> PluginResult<Self> {
        let mut account = Self::parse(value)?;
        if let Some(path) = &account.source {
            let (path, original) = (path.clone(), account.email.clone());
            let (key, server) = tokio::task::spawn_blocking(move || read(&path))
                .await
                .map_err(|_| expired())??;
            let who = network::user(client, &key, &server).await?;
            let (email, plan) = identity(&who)?;
            if original.is_empty() || !original.eq_ignore_ascii_case(&email) {
                return Err(expired());
            }
            account.key = key;
            account.server = server;
            account.plan = plan;
        }
        Ok(account)
    }
    pub fn credentials(&self) -> Value {
        let mut value = json!({"type":if self.source.is_some(){"oauth"}else{"api"},"authMethod":if self.source.is_some(){"cliSource"}else{"browser"},"accountId":self.email,"label":self.email,"metadata":{"email":self.email,"plan":self.plan,"server":self.server},"expires":0});
        if let Some(path) = &self.source {
            value["source"] = json!({"path":path,"kind":"file"});
        } else {
            value["key"] = json!(self.key);
        }
        value
    }
}
pub(crate) fn identity(value: &Value) -> PluginResult<(String, String)> {
    let status = &value["userStatus"];
    let email = text(status.get("email"), 512)
        .or_else(|| text(status.get("name"), 512))
        .ok_or_else(|| PluginError::authentication("Devin did not identify this account."))?;
    let info = &status["planStatus"]["planInfo"];
    let raw = text(status.get("teamsTier"), 256)
        .or_else(|| text(info.get("teamsTier"), 256))
        .unwrap_or_default();
    let plan = raw
        .trim_start_matches("TEAMS_TIER_")
        .split('_')
        .filter(|s| {
            !s.is_empty()
                && *s != "UNSPECIFIED"
                && !(s.starts_with('V') && s[1..].bytes().all(|b| b.is_ascii_digit()))
        })
        .map(|s| {
            let mut chars = s.chars();
            format!(
                "{}{}",
                chars.next().unwrap(),
                chars.as_str().to_ascii_lowercase()
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    Ok((
        email.into(),
        if plan.is_empty() {
            text(info.get("planName"), 256).unwrap_or_default().into()
        } else {
            plan
        },
    ))
}
pub(crate) fn read(path: &str) -> PluginResult<(String, String)> {
    if !Path::new(path).is_absolute() || path.len() > 16 * 1024 {
        return Err(PluginError::invalid(
            "Select an absolute Devin credentials.toml path.",
        ));
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|_| expired())?;
    if !metadata.is_file() || metadata.len() > 128 * 1024 {
        return Err(expired());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| expired())?;
    if !file.metadata().map_err(|_| expired())?.is_file() {
        return Err(expired());
    }
    let mut bytes = Vec::new();
    file.take(128 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| expired())?;
    if bytes.len() > 128 * 1024 {
        return Err(expired());
    }
    let value: toml::Value = toml::from_str(std::str::from_utf8(&bytes).map_err(|_| expired())?)
        .map_err(|_| expired())?;
    let key = value
        .get("windsurf_api_key")
        .and_then(toml::Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 64 * 1024 && !s.chars().any(char::is_whitespace))
        .ok_or_else(expired)?
        .to_owned();
    let server = server(
        value
            .get("api_server_url")
            .and_then(toml::Value::as_str)
            .unwrap_or(network::SERVER),
    )?;
    Ok((key, server))
}
#[cfg(test)]
#[path = "../test/account.test.rs"]
mod tests;
