use super::network;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use nora_provider_subscription_core as core;
use serde_json::{json, Value};
pub(crate) fn text(value: Option<&Value>, max: usize) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control))
}
pub(crate) fn expired() -> PluginError {
    PluginError::authentication("Factory sign-in expired or changed. Reconnect this account.")
}
#[derive(Clone)]
pub(crate) struct Account {
    pub key: bool,
    pub access: String,
    pub refresh: String,
    pub expires: u64,
    pub org: String,
    pub active_org: String,
    pub region: String,
    pub prem: String,
    pub email: String,
    pub user: String,
    pub saved: Value,
}
impl Account {
    pub fn parse(value: &Value) -> PluginResult<Self> {
        let key = value["type"] == "api";
        let access = text(
            if key {
                value.get("key")
            } else {
                value.get("access")
            },
            64 * 1024,
        )
        .filter(|s| !s.chars().any(char::is_whitespace))
        .ok_or_else(expired)?;
        let metadata = if key {
            value.get("metadata").unwrap_or(value)
        } else {
            value
        };
        let region = text(metadata.get("region"), 16).unwrap_or_default();
        if !["", "us", "eu"].contains(&region) {
            return Err(PluginError::invalid(
                "The Factory account region is unsupported.",
            ));
        }
        let account = Self {
            key,
            access: access.into(),
            refresh: if key {
                String::new()
            } else {
                text(value.get("refresh"), 64 * 1024)
                    .unwrap_or_default()
                    .into()
            },
            expires: if key {
                0
            } else {
                value["expires"].as_u64().unwrap_or(0)
            },
            org: text(value.get("orgId"), 256).unwrap_or_default().into(),
            active_org: if key {
                String::new()
            } else {
                text(value.get("activeOrganizationId"), 256)
                    .unwrap_or_default()
                    .into()
            },
            region: region.into(),
            prem: text(metadata.get("premBaseHost"), 2048)
                .unwrap_or_default()
                .into(),
            email: text(metadata.get("email"), 512).unwrap_or_default().into(),
            user: text(metadata.get("userId"), 256).unwrap_or_default().into(),
            saved: value.clone(),
        };
        account.llm_base()?;
        Ok(account)
    }
    pub fn base(&self) -> &'static str {
        if self.region == "eu" {
            network::EU
        } else {
            network::API
        }
    }
    pub fn llm_base(&self) -> PluginResult<String> {
        if self.prem.is_empty() {
            return Ok(self.base().into());
        }
        let raw = if self.prem.contains("://") {
            self.prem.clone()
        } else {
            format!("https://{}", self.prem)
        };
        let url = url::Url::parse(&raw)
            .map_err(|_| PluginError::invalid("Factory returned an invalid on-prem host."))?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.path(), "" | "/")
        {
            return Err(PluginError::invalid(
                "Factory on-prem requests require an HTTPS host without credentials or paths.",
            ));
        }
        Ok(url.as_str().trim_end_matches('/').into())
    }
    pub fn identity(&self) -> &str {
        if !self.user.is_empty() {
            &self.user
        } else {
            &self.email
        }
    }
    pub fn credentials(&self) -> Value {
        if self.key {
            return json!({"type":"api","authMethod":"apiKey","key":self.access,"accountId":self.identity(),"label":if self.email.is_empty(){&self.user}else{&self.email},"expires":0,"metadata":{"email":self.email,"userId":self.user,"region":self.region,"premBaseHost":self.prem}});
        }
        let mut value = self.saved.clone();
        for (k, v) in [
            ("access", &self.access),
            ("refresh", &self.refresh),
            ("orgId", &self.org),
            ("activeOrganizationId", &self.active_org),
            ("region", &self.region),
            ("premBaseHost", &self.prem),
            ("email", &self.email),
            ("userId", &self.user),
        ] {
            value[k] = json!(v);
        }
        value["type"] = json!("oauth");
        value["authMethod"] = json!("device");
        value["expires"] = json!(self.expires);
        value["accountId"] = json!(self.identity());
        value["label"] = json!(if self.email.is_empty() {
            &self.user
        } else {
            &self.email
        });
        value
    }
    pub fn apply_identity(&mut self, who: &Value) -> PluginResult<bool> {
        let user = text(who.get("userId"), 256).unwrap_or_default();
        let email = text(who.get("email"), 512).unwrap_or_default();
        if !self.user.is_empty() && !user.is_empty() && self.user != user
            || self.user.is_empty()
                && !self.email.is_empty()
                && !email.is_empty()
                && !self.email.eq_ignore_ascii_case(email)
        {
            return Err(expired());
        }
        let previous = self.credentials();
        if !user.is_empty() {
            self.user = user.into();
        }
        if !email.is_empty() {
            self.email = email.into();
        }
        self.region = text(who.get("region"), 16).unwrap_or_default().into();
        if !["", "us", "eu"].contains(&self.region.as_str()) {
            return Err(PluginError::invalid(
                "Factory returned an unsupported region.",
            ));
        }
        self.prem = text(who.get("premBaseHostV2"), 2048)
            .unwrap_or_default()
            .into();
        self.llm_base()?;
        if !self.key {
            self.active_org = text(who.get("orgId"), 256).unwrap_or_default().into();
        }
        Ok(previous != self.credentials())
    }
}
pub(crate) fn expires(access: &str) -> u64 {
    core::decode_jwt(access)["exp"]
        .as_u64()
        .unwrap_or(0)
        .saturating_mul(1000)
}
pub(crate) fn from_token(token: &Value) -> PluginResult<Account> {
    let access = text(token.get("access_token"), 64 * 1024).ok_or_else(expired)?;
    let claims = core::decode_jwt(access);
    let user = text(token.pointer("/user/id"), 256)
        .or_else(|| text(claims.get("sub"), 256))
        .unwrap_or_default();
    let email = text(token.pointer("/user/email"), 512)
        .or_else(|| text(claims.get("email"), 512))
        .unwrap_or_default();
    let org = text(token.get("organization_id"), 256)
        .or_else(|| text(claims.get("org_id"), 256))
        .unwrap_or_default();
    Account::parse(
        &json!({"type":"oauth","access":access,"refresh":token["refresh_token"],"expires":expires(access),"orgId":org,"userId":user,"email":email}),
    )
}
#[cfg(test)]
#[path = "../test/account.test.rs"]
mod tests;
