use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Default, Debug)]
pub(super) struct Refusals {
    accounts: Mutex<BTreeMap<String, Arc<Account>>>,
}

#[derive(Default, Debug)]
pub(super) struct Account {
    models: Mutex<BTreeMap<String, Instant>>,
}

impl Refusals {
    pub(super) fn account(&self, credentials: &Value) -> Arc<Account> {
        let key = identity(credentials);
        let mut accounts = self
            .accounts
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !accounts.contains_key(&key) && accounts.len() >= 64 {
            if let Some(first) = accounts.keys().next().cloned() {
                accounts.remove(&first);
            }
        }
        accounts.entry(key).or_default().clone()
    }
}

impl Account {
    pub(super) fn contains(&self, model: &str) -> bool {
        let mut models = self
            .models
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        models.retain(|_, until| *until > Instant::now());
        models.contains_key(model)
    }

    pub(super) fn remember(&self, model: &str) {
        if model.is_empty() || model == "auto" || model.len() > 512 {
            return;
        }
        let mut models = self
            .models
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        models.retain(|_, until| *until > Instant::now());
        if !models.contains_key(model) && models.len() >= 4096 {
            if let Some(first) = models.keys().next().cloned() {
                models.remove(&first);
            }
        }
        models.insert(model.into(), Instant::now() + Duration::from_secs(6 * 3600));
    }
}

pub(super) fn identity(credentials: &Value) -> String {
    let value = credentials
        .get("accountIdentity")
        .or_else(|| credentials.pointer("/extras/accountIdentity"))
        .filter(|identity| {
            identity
                .get("issuer")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty())
                && identity
                    .get("subject")
                    .and_then(Value::as_str)
                    .is_some_and(|value| !value.is_empty())
        })
        .unwrap_or(credentials);
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).unwrap_or_default())
    )
}

pub(super) fn model_refused(body: &[u8]) -> bool {
    let body = String::from_utf8_lossy(body).to_ascii_lowercase();
    body.contains("\"model_not_supported\"") || body.contains("requested model is not supported")
}

#[cfg(test)]
#[path = "../test/refusals.test.rs"]
mod tests;
