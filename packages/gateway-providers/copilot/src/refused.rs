//! The models Copilot refused an account picked by hand. Its model list
//! names models a Free plan is only served through Auto: each is learnt the
//! first time Copilot refuses it, and is left out of the account's list
//! from then on.

use provider_support::random_hex;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Where they are kept, in the plugin's own folder.
pub(crate) const FILE: &str = "copilot-refused.json";

#[derive(Default)]
pub(crate) struct Refused {
    path: Option<PathBuf>,
    by_account: BTreeMap<String, BTreeSet<String>>,
}

impl Refused {
    /// Those kept at `path`, or none.
    pub(crate) fn load(path: Option<PathBuf>) -> Self {
        let by_account = path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self { path, by_account }
    }

    pub(crate) fn contains(&self, account: &str, model: &str) -> bool {
        self.by_account
            .get(account)
            .is_some_and(|models| models.contains(model))
    }

    /// Notes the account was refused model, and keeps it.
    pub(crate) fn add(&mut self, account: &str, model: &str) {
        let added = self
            .by_account
            .entry(account.to_owned())
            .or_default()
            .insert(model.to_owned());
        if added {
            self.save();
        }
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let Ok(bytes) = serde_json::to_vec(&self.by_account) else {
            return;
        };
        let staged = path.with_extension(format!("{}.tmp", random_hex(4)));
        if std::fs::write(&staged, bytes).is_ok() {
            let _ = std::fs::rename(&staged, path);
        }
        let _ = std::fs::remove_file(staged);
    }
}

/// Whether Copilot's answer refuses the model the request named.
pub(crate) fn refusal(status: u16, value: &Value) -> bool {
    status == 400 && value["error"]["code"] == "model_not_supported"
}

/// The error a request for a model the account may not pick answers.
pub(crate) fn explained(model: &str) -> Value {
    serde_json::json!({"error": {
        "message": format!(
            "This account can't pick {model} by hand. Pick Auto, which Copilot serves its models through, or GPT-4.1."
        ),
        "type": "invalid_request_error",
        "code": "model_not_supported",
    }})
}

#[cfg(test)]
#[path = "../test/refused.test.rs"]
mod tests;
