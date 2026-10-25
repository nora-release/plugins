use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

mod account_import;
mod auth;
mod envelope;
mod models;
mod project;
mod provider;
mod session;
mod stream;
mod text_call;
mod usage;

pub use provider::CodeAssist;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum App {
    GeminiCLI,
    Antigravity,
}
impl App {
    pub fn id(self) -> &'static str {
        match self {
            Self::GeminiCLI => "gemini-cli",
            Self::Antigravity => "antigravity",
        }
    }
    pub fn base(self) -> &'static str {
        match self {
            Self::GeminiCLI => PROD,
            Self::Antigravity => "https://daily-cloudcode-pa.googleapis.com",
        }
    }
    fn client_id(self) -> &'static str {
        match self {
            Self::GeminiCLI => {
                "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com"
            }
            Self::Antigravity => {
                "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com"
            }
        }
    }
    fn client_secret(self) -> &'static str {
        match self {
            Self::GeminiCLI => "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",
            Self::Antigravity => "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",
        }
    }
    fn scopes(self) -> String {
        let mut scopes = vec![
            "https://www.googleapis.com/auth/cloud-platform",
            "https://www.googleapis.com/auth/userinfo.email",
            "https://www.googleapis.com/auth/userinfo.profile",
        ];
        if self == Self::Antigravity {
            scopes.extend([
                "https://www.googleapis.com/auth/cclog",
                "https://www.googleapis.com/auth/experimentsandconfigs",
            ]);
        }
        scopes.join(" ")
    }
    fn user_agent_with_version(self, model: &str, version: &str) -> String {
        let platform = match std::env::consts::OS {
            "macos" => "darwin",
            platform => platform,
        };
        let arch = match std::env::consts::ARCH {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            arch => arch,
        };
        if self == Self::Antigravity {
            return format!("antigravity/hub/{version} {platform}/{arch}");
        }
        let platform = if platform == "windows" {
            "win32"
        } else {
            platform
        };
        let arch = if arch == "amd64" { "x64" } else { arch };
        format!(
            "GeminiCLI/0.61.0/{} ({platform}; {arch}; terminal)",
            if model.is_empty() {
                "gemini-2.5-pro"
            } else {
                model
            }
        )
    }
}
const PROD: &str = "https://cloudcode-pa.googleapis.com";
const VERSION: &str = "https://antigravity-hub-auto-updater-974169037036.us-central1.run.app/manifest/latest-arm64-mac.yml";
const TOKEN: &str = "https://oauth2.googleapis.com/token";
const USER_INFO: &str = "https://www.googleapis.com/oauth2/v2/userinfo?alt=json";
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
fn invalid(message: &str) -> PluginError {
    PluginError::invalid(message)
}
fn expired() -> PluginError {
    PluginError::authentication(
        "The Google sign-in is unavailable or changed. Reconnect this account.",
    )
}
fn text(value: Option<&Value>, max: usize) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| {
            !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
        })
}
fn token(value: Option<&Value>) -> PluginResult<String> {
    text(value, 64 * 1024)
        .filter(|value| !value.chars().any(char::is_whitespace))
        .map(str::to_owned)
        .ok_or_else(expired)
}
fn fingerprint(value: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn profile(tokens: &session::Tokens) -> Value {
    json!({"email":tokens.email,"name":tokens.name,"accountId":tokens.uid})
}
