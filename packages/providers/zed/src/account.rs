use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
pub(crate) fn text(value: Option<&Value>, max: usize) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control))
}
pub(crate) fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
pub(crate) fn random() -> String {
    nora_ai::oauth_pkce::random_hex(24)
}
pub(crate) fn uuid() -> String {
    let raw = nora_ai::oauth_pkce::random_hex(16);
    format!(
        "{}-{}-4{}-8{}-{}",
        &raw[..8],
        &raw[8..12],
        &raw[13..16],
        &raw[17..20],
        &raw[20..]
    )
}
pub(crate) fn expired() -> PluginError {
    PluginError::authentication(
        "The selected Zed sign-in expired or changed. Reconnect this account.",
    )
}
#[derive(Clone)]
pub(crate) struct Account {
    pub user: String,
    pub access: String,
    pub system: String,
    pub org: String,
    pub plan: String,
    pub label: String,
    pub saved: Value,
}
impl Account {
    pub fn parse(value: &Value) -> PluginResult<Self> {
        let user = text(value.get("userID").or_else(|| value.get("userId")), 128)
            .filter(|s| {
                s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            })
            .ok_or_else(expired)?;
        let access = text(
            value.get("access").or_else(|| value.get("accessToken")),
            64 * 1024,
        )
        .filter(|s| !s.chars().any(char::is_whitespace))
        .ok_or_else(expired)?;
        Ok(Self {
            user: user.into(),
            access: access.into(),
            system: text(value.get("systemID").or_else(|| value.get("systemId")), 128)
                .unwrap_or_default()
                .into(),
            org: text(
                value.get("organizationID").or_else(|| value.get("org")),
                256,
            )
            .unwrap_or_default()
            .into(),
            plan: text(value.get("plan"), 128).unwrap_or_default().into(),
            label: text(value.get("label"), 512).unwrap_or_default().into(),
            saved: value.clone(),
        })
    }
    pub fn credentials(&self) -> Value {
        let mut value = self.saved.clone();
        value["type"] = json!("oauth");
        value["userID"] = json!(self.user);
        value["access"] = json!(self.access);
        value["systemID"] = json!(self.system);
        value["organizationID"] = json!(self.org);
        value["plan"] = json!(self.plan);
        value["label"] = json!(self.label);
        value["accountId"] = json!(self.user);
        value["expires"] = json!(0);
        value
    }
    pub fn token_key(&self) -> String {
        hash(&format!("{}\0{}\0{}", self.user, self.org, self.access))
    }
    pub fn authorization(&self) -> String {
        format!("{} {}", self.user, self.access)
    }
}
pub(crate) fn org(me: &Value, saved: &str) -> String {
    let orgs: Vec<_> = me["organizations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|o| text(o.get("id"), 256).is_some())
        .collect();
    let keep = orgs.iter().copied().find(|o| o["id"] == saved);
    let default = orgs
        .iter()
        .copied()
        .find(|o| o["id"] == me["default_organization_id"]);
    let usable = |o: &&Value| {
        let id = o["id"].as_str().unwrap();
        let plan = me["plans_by_organization"][id].as_str().unwrap_or_default();
        !plan.is_empty() && plan != "zed_free" && !disabled(me, id)
    };
    keep.filter(usable)
        .or_else(|| default.filter(usable))
        .or_else(|| {
            orgs.iter()
                .copied()
                .find(|o| o["is_personal"] == true && usable(o))
        })
        .or_else(|| orgs.iter().copied().find(usable))
        .or(keep)
        .or(default)
        .or_else(|| orgs.first().copied())
        .and_then(|o| o["id"].as_str())
        .unwrap_or(if !saved.is_empty() {
            saved
        } else {
            me["default_organization_id"].as_str().unwrap_or_default()
        })
        .into()
}
pub(crate) fn disabled(me: &Value, org: &str) -> bool {
    me["configuration_by_organization"][org]["is_zed_model_provider_enabled"] == false
}
pub(crate) fn plan(me: &Value, org: &str) -> String {
    if !org.is_empty()
        && me["organizations"]
            .as_array()
            .is_some_and(|o| o.iter().any(|o| o["id"] == org))
        && me["plans_by_organization"].is_object()
    {
        return me["plans_by_organization"][org]
            .as_str()
            .unwrap_or_default()
            .into();
    }
    me.pointer("/plan/plan_v3")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .into()
}
pub(crate) fn who(me: &Value) -> String {
    ["github_login", "username", "name"]
        .into_iter()
        .find_map(|key| text(me["user"].get(key), 512))
        .map(str::to_owned)
        .or_else(|| {
            me.pointer("/user/legacy_user_id")
                .filter(|v| v.is_number() || v.is_string())
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| v.to_string())
                })
        })
        .unwrap_or_default()
}
pub(crate) fn plan_name(plan: &str) -> &str {
    match plan {
        "zed_pro" => "Pro",
        "zed_pro_trial" => "Pro Trial",
        "zed_business" => "Business",
        "zed_student" => "Student",
        "zed_vip" => "VIP",
        "" | "zed_free" => "No plan",
        v => v.strip_prefix("zed_").unwrap_or(v),
    }
}
#[cfg(test)]
#[path = "../test/account.test.rs"]
mod tests;
