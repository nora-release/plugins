use super::{session::Credential, source_kind::MagpieAccount};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub(crate) const MAX_BYTES: u64 = 16 * 1024 * 1024;
fn unavailable() -> PluginError {
    PluginError::authentication("The saved Magpie Kiro sign-in is unavailable or changed.")
}
pub(crate) fn default_path() -> PluginResult<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(unavailable)?;
    Ok(default_path_at(&home, &std::env::vars().collect()))
}
fn default_path_at(home: &Path, environment: &BTreeMap<String, String>) -> PathBuf {
    environment
        .get("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
        .join("magpie/plugin-auth.json")
}
fn owned(value: &Value) -> bool {
    value.is_object()
        && value["type"] == "oauth"
        && match value.get("source") {
            None | Some(Value::Null) => true,
            Some(Value::String(source)) => source.is_empty(),
            _ => false,
        }
}
pub(crate) fn document(bytes: &[u8]) -> PluginResult<Value> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(unavailable());
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| unavailable())?;
    if !value.is_object() {
        return Err(unavailable());
    }
    Ok(value)
}
pub(crate) fn read_document(path: &Path) -> PluginResult<Option<Value>> {
    if !path.is_absolute() {
        return Err(unavailable());
    }
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(unavailable()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable())?;
    document(&bytes).map(Some)
}
pub(crate) fn list_sources(path: &Path) -> PluginResult<Vec<MagpieAccount>> {
    let Some(value) = read_document(path)? else {
        return Ok(Vec::new());
    };
    let mut sources = value
        .as_object()
        .ok_or_else(unavailable)?
        .iter()
        .filter(|(_, value)| owned(value))
        .filter_map(|(key, _)| MagpieAccount::new(key).ok())
        .collect::<Vec<_>>();
    sources.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    Ok(sources)
}
pub(crate) fn credential(
    account: &MagpieAccount,
    document: &Value,
) -> PluginResult<Option<Credential>> {
    let Some(value) = document.get(account.as_str()) else {
        return Ok(None);
    };
    if !owned(value) {
        return Ok(None);
    }
    let mut value = value.clone();
    let fields = value.as_object_mut().ok_or_else(unavailable)?;
    // Magpie's accountId is a display name and uid is the login method.
    // Neither is the legacy Nora fingerprint or a verified Kiro user id.
    fields.remove("accountId");
    fields.remove("uid");
    Credential::from_saved(&value).map(Some)
}
pub(crate) fn same_credentials(left: &Value, right: &Value) -> bool {
    const FIELDS: [&str; 15] = [
        "type",
        "access",
        "refresh",
        "expires",
        "method",
        "profileArn",
        "region",
        "clientId",
        "clientSecret",
        "tokenEndpoint",
        "realm",
        "builderID",
        "loginProvider",
        "userId",
        "accountIdentity",
    ];
    owned(left)
        && owned(right)
        && FIELDS
            .into_iter()
            .all(|field| left.get(field) == right.get(field))
        && left.pointer("/extras/accountIdentity") == right.pointer("/extras/accountIdentity")
}
pub(crate) fn read_credential(
    account: &MagpieAccount,
    path: &Path,
) -> PluginResult<Option<Credential>> {
    let Some(value) = read_document(path)? else {
        return Ok(None);
    };
    credential(account, &value)
}

pub(crate) struct WriteLock {
    path: PathBuf,
    nonce: Vec<u8>,
}
impl WriteLock {
    pub(crate) fn acquire(path: &Path) -> PluginResult<Self> {
        let path = PathBuf::from(format!("{}.lock", path.to_string_lossy()));
        let nonce = nora_ai::oauth_pkce::random_hex(16).into_bytes();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(mut file) => {
                    let lock = Self { path, nonce };
                    if file.write_all(&lock.nonce).is_err() {
                        drop(file);
                        let _ = std::fs::remove_file(&lock.path);
                        return Err(unavailable());
                    }
                    return Ok(lock);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if std::time::Instant::now() >= deadline {
                        return Err(unavailable());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                Err(_) => return Err(unavailable()),
            }
        }
    }
    pub(crate) fn still_owned(&self) -> bool {
        let Ok(file) = std::fs::File::open(&self.path) else {
            return false;
        };
        let mut bytes = Vec::new();
        file.take(self.nonce.len() as u64 + 1)
            .read_to_end(&mut bytes)
            .is_ok()
            && bytes == self.nonce
    }
}
impl Drop for WriteLock {
    fn drop(&mut self) {
        if self.still_owned() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
#[path = "../test/magpie.test.rs"]
mod tests;
