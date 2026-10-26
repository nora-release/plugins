use nora_ai::{OAuthCredentials, ResolvedProviderAuth};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::RwLock,
};

#[path = "rate_limit_store.rs"]
mod store;

const HOLD_MS: i64 = 5 * 60_000;
const ACCOUNT_KEY: &str = "grokRateLimitAccount";

#[derive(Debug, Default)]
pub(super) struct RateLimits(RwLock<Option<PathBuf>>);

fn account(value: &Value) -> Option<String> {
    let field = |key: &str| {
        value
            .get("extras")
            .and_then(|extras| extras.get(key))
            .or_else(|| value.get(key))
    };
    let identity = field("accountIdentity");
    let user = field("userId")
        .and_then(Value::as_str)
        .or_else(|| identity?.get("subject")?.as_str())?;
    if user.is_empty() || user.len() > 512 || user.chars().any(char::is_control) {
        return None;
    }
    let scope = identity
        .and_then(|identity| identity.get("scope"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            serde_json::to_string(&[
                field("teamId").and_then(Value::as_str).unwrap_or_default(),
                field("organizationId")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ])
            .unwrap()
        });
    if scope.len() > 512 || scope.chars().any(char::is_control) {
        return None;
    }
    Some(serde_json::to_string(&[user, &scope]).unwrap())
}

pub(super) fn bind_auth(credentials: &OAuthCredentials, auth: &mut ResolvedProviderAuth) {
    if let Ok(value) = serde_json::to_value(credentials) {
        if let Some(key) = account(&value) {
            auth.metadata.insert(ACCOUNT_KEY.into(), key.into());
        }
    }
}

fn header<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn seconds_after(value: &str, now: i64) -> Option<i64> {
    let seconds = value.trim().parse::<f64>().ok()?;
    if !seconds.is_finite() || seconds < 0.0 || seconds > (i64::MAX - now) as f64 / 1000.0 {
        return None;
    }
    now.checked_add((seconds * 1000.0) as i64)
}

fn reset(headers: &BTreeMap<String, String>, now: i64) -> Option<i64> {
    if let Some(value) = header(headers, "retry-after") {
        if let Some(at) = seconds_after(value, now).or_else(|| {
            chrono::DateTime::parse_from_rfc2822(value)
                .ok()
                .map(|date| date.timestamp_millis())
        }) {
            return Some(at);
        }
    }
    for (key, value) in headers {
        let key = key.to_ascii_lowercase();
        let Some((_, suffix)) = key.split_once("ratelimit") else {
            continue;
        };
        if !suffix.contains("reset") {
            continue;
        }
        let Some(value) = value
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && *value >= 0.0)
        else {
            continue;
        };
        let at = if value > 1e12 {
            value
        } else if value > 1e9 {
            value * 1000.0
        } else {
            now as f64 + value * 1000.0
        };
        if at < i64::MAX as f64 {
            return Some(at as i64);
        }
    }
    None
}

impl RateLimits {
    pub(super) fn initialize(
        &self,
        directory: &Path,
    ) -> nora_provider_plugin_sdk::PluginResult<()> {
        let directory = store::directory(directory).map_err(|_| {
            nora_provider_plugin_sdk::PluginError::invalid(
                "The Grok rate-limit store is unavailable.",
            )
        })?;
        *self.0.write().map_err(|_| {
            nora_provider_plugin_sdk::PluginError::invalid(
                "The Grok rate-limit store is unavailable.",
            )
        })? = Some(directory);
        Ok(())
    }
    pub(super) fn observe(
        &self,
        credentials: &Value,
        status: u16,
        headers: &BTreeMap<String, String>,
        now: i64,
    ) {
        if !nora_provider::adapter::is_oauth(Some(credentials)) {
            return;
        }
        let Some(key) = account(credentials) else {
            return;
        };
        let Ok(directory) = self.0.read() else {
            return;
        };
        let Some(directory) = directory.as_ref() else {
            return;
        };
        if status == 429 {
            let until = reset(headers, now).unwrap_or(now + HOLD_MS);
            let _ = store::update(directory, &key, (until > now).then_some(until));
        } else if (200..300).contains(&status) {
            let _ = store::update(directory, &key, None);
        }
    }

    pub(super) fn apply(&self, auth: &ResolvedProviderAuth, usage: &mut Value, now: i64) {
        let Some(key) = auth.metadata.get(ACCOUNT_KEY).and_then(|key| key.as_str()) else {
            return;
        };
        let Ok(directory) = self.0.read() else {
            return;
        };
        let Some(directory) = directory.as_ref() else {
            return;
        };
        let Some(until) = store::read(directory, key, now).ok().flatten() else {
            return;
        };
        let Some(at) = chrono::DateTime::from_timestamp_millis(until) else {
            return;
        };
        let Some(items) = usage.get_mut("items").and_then(Value::as_array_mut) else {
            return;
        };
        items.insert(0, json!({"id":"rate_limit","label":"Rate limit","value":"100% used","usedPercent":100.0,"countsForRouting":false,"resetsAt":at.to_rfc3339()}));
        nora_provider::usage::parsing::update_availability(usage);
    }
}

#[cfg(test)]
#[path = "../test/rate_limit_state.test.rs"]
mod tests;
