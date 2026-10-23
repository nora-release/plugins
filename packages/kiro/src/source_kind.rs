use crate::session;
use nora_provider_plugin_sdk::PluginResult;
use serde_json::{json, Value};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MagpieAccount(String);

impl MagpieAccount {
    pub(crate) fn new(value: &str) -> PluginResult<Self> {
        if value != "kiro" {
            let suffix = value.strip_prefix("kiro#").ok_or_else(session::expired)?;
            if suffix.is_empty()
                || value.len() > 128
                || !suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            {
                return Err(session::expired());
            }
        }
        Ok(Self(value.to_owned()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SourceKind {
    Cli,
    Ide,
    Magpie(MagpieAccount),
}

impl SourceKind {
    pub(crate) fn from_reference(value: &Value) -> PluginResult<Self> {
        match session::text(value.get("kind"), 16) {
            Some("cli") => Ok(Self::Cli),
            Some("ide") => Ok(Self::Ide),
            Some("magpie") => Ok(Self::Magpie(MagpieAccount::new(
                session::text(value.get("account"), 128).ok_or_else(session::expired)?,
            )?)),
            _ => Err(session::expired()),
        }
    }

    pub(crate) fn from_auth_method(method: &str, inputs: &Value) -> PluginResult<Self> {
        match method {
            "cliSource" => Ok(Self::Cli),
            "ideSource" => Ok(Self::Ide),
            "magpieSource" => Ok(Self::Magpie(MagpieAccount::new(
                session::text(inputs.get("sourceAccount"), 128).ok_or_else(session::expired)?,
            )?)),
            _ => Err(session::expired()),
        }
    }

    pub(crate) fn auth_method(&self) -> &'static str {
        match self {
            Self::Cli => "cliSource",
            Self::Ide => "ideSource",
            Self::Magpie(_) => "magpieSource",
        }
    }

    /// All Magpie accounts share one credential document and write lock.
    pub(crate) fn storage_kind(&self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Ide => "ide",
            Self::Magpie(_) => "magpie",
        }
    }

    pub(crate) fn cache_key(&self) -> String {
        match self {
            Self::Magpie(account) => format!("magpie:{}", account.as_str()),
            _ => self.storage_kind().to_owned(),
        }
    }

    pub(crate) fn reference(&self, path: &str) -> Value {
        let mut value = json!({"kind":self.storage_kind(),"path":path});
        if let Self::Magpie(account) = self {
            value["account"] = json!(account.as_str());
        }
        value
    }

    pub(crate) fn marker(&self, path: &str) -> Value {
        let mut value = self.reference(path);
        value["kind"] = json!(self.auth_method());
        value
    }
}

#[cfg(test)]
#[path = "../test/source_kind.test.rs"]
mod tests;
