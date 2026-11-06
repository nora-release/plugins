use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

const OPTIONAL_HEADERS: [&str; 2] = [
    "X-Vertex-AI-LLM-Request-Type",
    "X-Vertex-AI-LLM-Shared-Request-Type",
];

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Config {
    pub project: String,
    pub location: String,
    pub source_path: PathBuf,
    pub impersonate: String,
    pub identity: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

impl Config {
    pub fn from_inputs(inputs: &Value) -> PluginResult<Self> {
        let text = |key| inputs.get(key).and_then(Value::as_str).unwrap_or("").trim();
        let project = text("project").to_lowercase();
        let location = match text("location") {
            "" => "global".into(),
            value => value.to_lowercase(),
        };
        let impersonate = text("impersonate").to_lowercase();
        let value = Self {
            project,
            location,
            impersonate,
            source_path: credentials_file(text("sourcePath"))?,
            identity: String::new(),
            headers: optional_headers(inputs)?,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn from_saved(value: Option<&Value>) -> PluginResult<Self> {
        let config: Self = serde_json::from_value(value.cloned().ok_or_else(reconnect)?)
            .map_err(|_| reconnect())?;
        config.validate()?;
        if config.identity.len() != 64
            || !config.identity.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(reconnect());
        }
        Ok(config)
    }

    fn validate(&self) -> PluginResult<()> {
        if self.headers.len() > OPTIONAL_HEADERS.len()
            || self.headers.iter().any(|(name, value)| {
                !OPTIONAL_HEADERS
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(name))
                    || value.len() > 512
                    || value.chars().any(char::is_control)
            })
        {
            return Err(PluginError::invalid(
                "The optional Vertex request headers are invalid.",
            ));
        }
        if self.project.len() < 2
            || self.project.len() > 256
            || !self
                .project
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b".:-".contains(&b))
            || !self
                .project
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            || !self
                .project
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
        {
            return Err(PluginError::invalid(
                "Enter a valid Google Cloud project ID.",
            ));
        }
        if self.location.len() > 64
            || self.location.split('-').any(|part| {
                part.is_empty()
                    || !part
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            })
            || !self
                .location
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_lowercase)
        {
            return Err(PluginError::invalid(
                "Choose global, us, eu, or a Vertex region.",
            ));
        }
        if !self.source_path.is_absolute()
            || (!self.impersonate.is_empty() && !valid_account(&self.impersonate))
        {
            return Err(PluginError::invalid(
                "Use an absolute credentials path and a valid service account email.",
            ));
        }
        Ok(())
    }

    pub fn root(&self) -> String {
        let host = match self.location.as_str() {
            "global" => "aiplatform.googleapis.com".into(),
            "us" | "eu" => format!("aiplatform.{}.rep.googleapis.com", self.location),
            region => format!("{region}-aiplatform.googleapis.com"),
        };
        format!("https://{host}")
    }

    pub fn base(&self) -> String {
        format!(
            "{}/v1/projects/{}/locations/{}/publishers/google",
            self.root(),
            self.project,
            self.location
        )
    }
}

fn optional_headers(inputs: &Value) -> PluginResult<BTreeMap<String, String>> {
    let mut headers = BTreeMap::new();
    if let Some(values) = inputs.get("headers") {
        let values = values.as_object().ok_or_else(|| {
            PluginError::invalid("Vertex request headers must be named text values.")
        })?;
        for (name, value) in values {
            let canonical = OPTIONAL_HEADERS
                .iter()
                .find(|allowed| allowed.eq_ignore_ascii_case(name))
                .ok_or_else(|| {
                    PluginError::invalid(
                        "Only the declared optional Vertex request headers can be set.",
                    )
                })?;
            let value = value
                .as_str()
                .ok_or_else(|| PluginError::invalid("Vertex request headers must be text."))?;
            if value.chars().any(char::is_control) {
                return Err(PluginError::invalid(
                    "Vertex request headers cannot contain control characters.",
                ));
            }
            if !value.trim().is_empty() {
                headers.insert(canonical.to_string(), value.trim().into());
            }
        }
    }
    for name in OPTIONAL_HEADERS {
        if let Some(value) = inputs.get(format!("header:{name}")) {
            let value = value
                .as_str()
                .ok_or_else(|| PluginError::invalid("Vertex request headers must be text."))?;
            if value.chars().any(char::is_control) {
                return Err(PluginError::invalid(
                    "Vertex request headers cannot contain control characters.",
                ));
            }
            if !value.trim().is_empty() {
                headers.insert(name.into(), value.trim().into());
            }
        }
    }
    Ok(headers)
}

pub(crate) fn valid_account(value: &str) -> bool {
    value.len() <= 320
        && value.split('@').count() == 2
        && value.split('@').all(|part| !part.is_empty())
        && !value
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "/\\:?#%".contains(c))
}

fn absolute(value: &str) -> PluginResult<PathBuf> {
    let path = if value == "~" || value.starts_with("~/") {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| PluginError::invalid("The home directory is unavailable."))?;
        PathBuf::from(home).join(value.strip_prefix("~/").unwrap_or(""))
    } else {
        PathBuf::from(value)
    };
    if !path.is_absolute() {
        return Err(PluginError::invalid("Google credentials paths must be absolute. A configured path is never replaced by another account's file."));
    }
    Ok(path)
}

pub(crate) fn credentials_file(given: &str) -> PluginResult<PathBuf> {
    if !given.is_empty() {
        return absolute(given);
    }
    if let Some(value) =
        std::env::var_os("GOOGLE_APPLICATION_CREDENTIALS").filter(|v| !v.is_empty())
    {
        return absolute(&value.to_string_lossy());
    }
    let directory = match std::env::var_os("CLOUDSDK_CONFIG").filter(|v| !v.is_empty()) {
        Some(value) => absolute(&value.to_string_lossy())?,
        None => absolute("~/.config/gcloud")?,
    };
    Ok(directory.join("application_default_credentials.json"))
}

pub(crate) fn reconnect() -> PluginError {
    PluginError::authentication("The connected Google credentials changed or are unavailable. Reconnect this account; another account's credentials were not used.")
}

#[cfg(test)]
#[path = "../test/config.test.rs"]
mod tests;
