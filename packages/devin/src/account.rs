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
#[derive(Clone, Default)]
pub(crate) struct Profile {
    pub email: String,
    pub plan: String,
    pub organization: String,
    pub org_id: String,
    pub team_id: String,
}
impl Profile {
    fn saved(value: &Value) -> Self {
        let field = |key| {
            text(value.pointer(&format!("/metadata/{key}")), 512)
                .map(str::trim)
                .unwrap_or_default()
                .to_owned()
        };
        Self {
            email: text(value.get("accountId").or_else(|| value.get("email")), 512)
                .map(str::trim)
                .unwrap_or_else(|| text(value.pointer("/metadata/email"), 512).unwrap_or_default())
                .into(),
            plan: field("plan"),
            organization: field("organization"),
            org_id: field("orgId"),
            team_id: field("teamId"),
        }
    }
    pub fn display_name(&self) -> String {
        if self.organization.is_empty() || self.organization.eq_ignore_ascii_case(&self.email) {
            return self.email.clone();
        }
        format!("{} · {}", self.email, self.organization)
    }
    pub fn public(&self, id: &str) -> Value {
        let mut profile = json!({"id":id,"email":self.email,"name":self.email,"displayName":self.display_name(),"plan":self.plan});
        for (key, value) in [
            ("organization", &self.organization),
            ("orgId", &self.org_id),
            ("teamId", &self.team_id),
        ] {
            if !value.is_empty() {
                profile[key] = json!(value);
            }
        }
        profile
    }
    fn update(&mut self, next: Self) -> PluginResult<()> {
        if !self.email.is_empty() && !self.email.eq_ignore_ascii_case(&next.email) {
            return Err(expired());
        }
        self.email = next.email;
        if !next.plan.is_empty() {
            self.plan = next.plan;
        }
        self.organization = next.organization;
        if !next.org_id.is_empty() {
            self.org_id = next.org_id;
        }
        if !next.team_id.is_empty() {
            self.team_id = next.team_id;
        }
        Ok(())
    }
}
#[derive(Clone)]
pub(crate) struct Account {
    pub key: String,
    pub server: String,
    pub profile: Profile,
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
            profile: Profile::saved(value),
            source,
            saved: value.clone(),
        })
    }
    pub async fn live(value: &Value, client: &dyn HttpClient) -> PluginResult<Self> {
        if value.get("source").is_some() {
            return Self::status(value, client)
                .await
                .map(|(account, _)| account);
        }
        Self::parse(value)
    }
    pub async fn status(value: &Value, client: &dyn HttpClient) -> PluginResult<(Self, Value)> {
        let (mut account, who) = Self::read_status(value, client).await?;
        account.update_profile(&who)?;
        Ok((account, who))
    }
    pub async fn usage_status(
        value: &Value,
        client: &dyn HttpClient,
    ) -> PluginResult<(Self, Value)> {
        let (mut account, who) = Self::read_status(value, client).await?;
        match identity(&who) {
            Ok(profile) => account.profile.update(profile)?,
            // An owned key already belongs to this account. Quota-only replies
            // keep its profile; a mutable CLI source must identify its key.
            Err(error) if account.source.is_some() => return Err(error),
            Err(_) => {}
        }
        Ok((account, who))
    }
    async fn read_status(value: &Value, client: &dyn HttpClient) -> PluginResult<(Self, Value)> {
        let mut account = Self::parse(value)?;
        if let Some(path) = &account.source {
            let path = path.clone();
            let (key, server) = tokio::task::spawn_blocking(move || read(&path))
                .await
                .map_err(|_| expired())??;
            if account.profile.email.is_empty() {
                return Err(expired());
            }
            account.key = key;
            account.server = server;
        }
        let who = network::user(client, &account.key, &account.server).await?;
        Ok((account, who))
    }
    pub fn update_profile(&mut self, value: &Value) -> PluginResult<()> {
        self.profile.update(identity(value)?)
    }
    pub fn credentials(&self) -> Value {
        let profile = &self.profile;
        let mut value = json!({"type":if self.source.is_some(){"oauth"}else{"api"},"authMethod":if self.source.is_some(){"cliSource"}else{"browser"},"accountId":profile.email,"label":profile.display_name(),"displayName":profile.display_name(),"email":profile.email,"name":profile.email,"plan":profile.plan,"endpoint":self.server,"metadata":{"email":profile.email,"plan":profile.plan,"server":self.server,"profileVersion":1},"expires":0});
        for (key, field) in [
            ("organization", &profile.organization),
            ("orgId", &profile.org_id),
            ("teamId", &profile.team_id),
        ] {
            if !field.is_empty() {
                value["metadata"][key] = json!(field);
            }
        }
        if !profile.org_id.is_empty() {
            value["organizationId"] = json!(profile.org_id);
        }
        if !profile.team_id.is_empty() {
            value["workspaceId"] = json!(profile.team_id);
        }
        if let Some(path) = &self.source {
            value["source"] = json!({"path":path,"kind":"file"});
        } else {
            value["key"] = json!(self.key);
        }
        nora_provider::account_import::preserve_metadata(&self.saved, &mut value);
        value
    }
}
pub(crate) fn identity(value: &Value) -> PluginResult<Profile> {
    let status = &value["userStatus"];
    let clean = |value| {
        text(value, 512)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let email = clean(status.get("email"))
        .or_else(|| clean(status.get("name")))
        .ok_or_else(|| PluginError::authentication("Devin did not identify this account."))?;
    let info = &status["planStatus"]["planInfo"];
    let devin = &info["devinInfo"];
    let organization = clean(devin.get("accountDisplayName"))
        .or_else(|| clean(info.get("teamName")))
        .or_else(|| clean(status.get("teamName")))
        .unwrap_or_default();
    let team_id = clean(status.get("teamId")).unwrap_or_default();
    let org_id = clean(devin.get("orgId")).unwrap_or(team_id);
    let raw = clean(status.get("teamsTier"))
        .or_else(|| clean(info.get("teamsTier")))
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
    Ok(Profile {
        email: email.into(),
        plan: if plan.is_empty() {
            clean(info.get("planName")).unwrap_or_default().into()
        } else {
            plan
        },
        organization: organization.into(),
        org_id: org_id.into(),
        team_id: team_id.into(),
    })
}
pub(crate) fn read(path: &str) -> PluginResult<(String, String)> {
    nora_provider_plugin_sdk::require_subscription_accounts()?;
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
