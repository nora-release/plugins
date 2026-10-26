use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Settings {
    pub project: Option<String>,
    pub location: String,
    pub billing_account: Option<String>,
    pub profile: PathBuf,
    pub account: Option<String>,
    pub expected_subject: Option<String>,
    pub auth_mode: String,
    pub cloudsdk_config: Option<PathBuf>,
    pub adc_root: PathBuf,
    pub cache_root: PathBuf,
    pub force: bool,
    pub model_ttl: f64,
    pub credits_ttl: f64,
    pub failure_ttl: f64,
    pub transient_ttl: f64,
}

impl Settings {
    pub fn resolve(row: &Value, auth: Option<&Value>, directory: &Path) -> PluginResult<Self> {
        Self::resolve_with_env(row, auth, directory, &std::env::vars().collect())
    }
    pub(super) fn resolve_with_env(
        row: &Value,
        auth: Option<&Value>,
        directory: &Path,
        env: &BTreeMap<String, String>,
    ) -> PluginResult<Self> {
        let empty = Value::Null;
        let auth = auth.unwrap_or(&empty);
        let row = row
            .get("options")
            .filter(|options| options.is_object())
            .unwrap_or(row);
        let string = |value: Option<&Value>| {
            value
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        let auth_field = |key: &str| {
            string(auth.get(key))
                .or_else(|| string(auth.get("extras").and_then(|extras| extras.get(key))))
        };
        let row_field = |key: &str| string(row.get(key));
        let field = |key: &str| auth_field(key).or_else(|| row_field(key));
        let variable = |key: &str| env.get(key).filter(|value| !value.is_empty()).cloned();
        let home = variable("HOME")
            .or_else(|| variable("USERPROFILE"))
            .map(PathBuf::from)
            .ok_or_else(|| PluginError::invalid("The home directory is unavailable."))?;
        let config_root = variable("XDG_CONFIG_HOME")
            .or_else(|| {
                if cfg!(windows) {
                    variable("APPDATA")
                } else {
                    None
                }
            })
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let cache_root = variable("XDG_CACHE_HOME")
            .or_else(|| {
                if cfg!(windows) {
                    variable("LOCALAPPDATA")
                } else {
                    None
                }
            })
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cache"));
        let ttl = |key: &str, fallback: f64| -> PluginResult<f64> {
            let value = variable(key)
                .map(|value| value.parse::<f64>().unwrap_or(f64::NAN))
                .unwrap_or(fallback);
            if !value.is_finite() || value < 0.0 {
                return Err(PluginError::invalid(format!(
                    "{key} must be a nonnegative number of seconds."
                )));
            }
            Ok(value)
        };
        let profile = field("profile")
            .or_else(|| variable("VERTEX_CHROME_PROFILE"))
            .map(|path| expand_with_home(&path, &home))
            .unwrap_or_else(|| config_root.join("gcp-auto-profile"));
        let expected_subject = string(auth.pointer("/accountIdentity/subject"))
            .or_else(|| string(auth.pointer("/extras/accountIdentity/subject")));
        let settings = Self {
            project: field("project")
                .or_else(|| variable("GOOGLE_VERTEX_PROJECT"))
                .or_else(|| variable("GOOGLE_CLOUD_PROJECT")),
            location: field("location")
                .or_else(|| variable("GOOGLE_VERTEX_LOCATION"))
                .unwrap_or_else(|| "global".into()),
            billing_account: field("billingAccount").or_else(|| variable("VERTEX_BILLING_ACCOUNT")),
            profile,
            account: auth_field("accountId")
                .or_else(|| auth_field("account"))
                .or_else(|| {
                    expected_subject
                        .as_ref()
                        .filter(|subject| validate_account(subject).is_ok())
                        .cloned()
                })
                .or_else(|| row_field("account"))
                .or_else(|| variable("VERTEX_GCLOUD_ACCOUNT")),
            expected_subject,
            auth_mode: field("authMode")
                .or_else(|| variable("VERTEX_GCLOUD_AUTH_MODE"))
                .unwrap_or_else(|| "auto".into()),
            // CLOUDSDK_CONFIG is machine-wide; it must not pick every account's ADC.
            cloudsdk_config: field("cloudsdkConfig").map(|path| expand_with_home(&path, &home)),
            adc_root: directory.join("adc"),
            cache_root: cache_root.join("nora").join("gcp-ai"),
            force: auth.get("force").and_then(Value::as_bool).unwrap_or(false),
            model_ttl: ttl("VERTEX_MODELS_CACHE_TTL", 21600.0)?,
            credits_ttl: ttl("VERTEX_CREDITS_CACHE_TTL", 300.0)?,
            failure_ttl: ttl("VERTEX_CREDITS_FAILURE_TTL", 1200.0)?,
            transient_ttl: ttl("VERTEX_CREDITS_TRANSIENT_TTL", 60.0)?,
        };
        settings.validate()?;
        Ok(settings)
    }
    pub fn validate(&self) -> PluginResult<()> {
        if let Some(project) = &self.project {
            validate_project(project)?;
        }
        if let Some(account) = &self.account {
            validate_account(account)?;
        }
        if let Some(account) = &self.billing_account {
            validate_billing(account)?;
        }
        if !matches!(self.auth_mode.as_str(), "auto" | "adc" | "gcloud") {
            return Err(PluginError::invalid(
                "VERTEX_GCLOUD_AUTH_MODE must be auto, gcloud or adc.",
            ));
        }
        if self.location.is_empty()
            || self.location.split('-').any(|part| {
                part.is_empty()
                    || !part
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            })
        {
            return Err(PluginError::invalid("Invalid GOOGLE_VERTEX_LOCATION."));
        }
        Ok(())
    }
    pub fn endpoint(&self, project: &str) -> PluginResult<String> {
        validate_project(project)?;
        self.validate()?;
        let host = if self.location == "global" {
            "aiplatform.googleapis.com".into()
        } else {
            format!("{}-aiplatform.googleapis.com", self.location)
        };
        Ok(format!(
            "https://{host}/v1beta1/projects/{project}/locations/{}/publishers/google",
            self.location
        ))
    }
}
fn expand_with_home(value: &str, home: &Path) -> PathBuf {
    let path = value
        .strip_prefix("~/")
        .map(|suffix| home.join(suffix))
        .unwrap_or_else(|| PathBuf::from(value));
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| home.to_owned())
            .join(path)
    }
}
pub(crate) fn validate_project(project: &str) -> PluginResult<()> {
    if !(6..=63).contains(&project.len())
        || !project.starts_with(|c: char| c.is_ascii_lowercase())
        || !project.ends_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        || !project
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(PluginError::invalid(
            "Set GOOGLE_VERTEX_PROJECT to a Google Cloud project ID, not its display name.",
        ));
    }
    Ok(())
}
pub(crate) fn validate_account(account: &str) -> PluginResult<()> {
    let Some((name, domain)) = account.split_once('@') else {
        return Err(PluginError::invalid(
            "Choose the intended Google account email.",
        ));
    };
    if name.is_empty()
        || domain.is_empty()
        || account.starts_with('-')
        || account.chars().any(char::is_whitespace)
        || domain.contains('@')
    {
        return Err(PluginError::invalid(
            "Choose the intended Google account email.",
        ));
    }
    Ok(())
}
pub(crate) fn validate_billing(account: &str) -> PluginResult<()> {
    if account.split('-').count() != 3
        || account
            .split('-')
            .any(|part| part.len() != 6 || !part.chars().all(|c| c.is_ascii_hexdigit()))
    {
        return Err(PluginError::invalid(
            "Invalid billing account ID; expected XXXXXX-XXXXXX-XXXXXX.",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "../test/settings.test.rs"]
mod tests;
