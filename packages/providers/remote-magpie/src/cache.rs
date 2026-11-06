//! Disk-backed reads are shared by short-lived usage processes, keyed by the
//! gateway address and key fingerprint. No credentials are written to this cache.
use crate::transport::RemoteError;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
const TTL: u64 = 30_000;
const MAX_BYTES: u64 = 12 * 1024 * 1024;
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Reading {
    pub value: Option<Value>,
    pub error: String,
    pub held: bool,
    pub read_at: u64,
    pub checked_at: u64,
    revision: u64,
}
#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct State {
    next: u64,
    cards: Reading,
    key: Reading,
}
impl State {
    fn entry(&mut self, cards: bool) -> &mut Reading {
        if cards {
            &mut self.cards
        } else {
            &mut self.key
        }
    }
}
pub(crate) enum Begin {
    Cached(Reading),
    Read(u64),
}
pub(crate) struct Cache {
    path: PathBuf,
    lock: PathBuf,
}
impl Cache {
    pub fn new(directory: &Path, fingerprint: &str) -> PluginResult<Self> {
        if fingerprint.len() != 64 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(failed());
        }
        let directory = directory.join("remote-usage");
        std::fs::create_dir_all(&directory).map_err(|_| failed())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| failed())?;
        }
        Ok(Self {
            path: directory.join(format!("{fingerprint}.json")),
            lock: directory.join(format!("{fingerprint}.lock")),
        })
    }
    fn access<T>(&self, change: impl FnOnce(&mut State) -> PluginResult<T>) -> PluginResult<T> {
        let _lock = Lock::open(&self.lock)?;
        let mut state = match open(&self.path, false) {
            Ok(mut file) => {
                if file.metadata().map_err(|_| failed())?.len() > MAX_BYTES {
                    return Err(failed());
                }
                let mut bytes = Vec::new();
                Read::by_ref(&mut file)
                    .take(MAX_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_| failed())?;
                serde_json::from_slice(&bytes).map_err(|_| failed())?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(_) => return Err(failed()),
        };
        let result = change(&mut state)?;
        let bytes = serde_json::to_vec(&state).map_err(|_| failed())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(failed());
        }
        let temporary = self
            .path
            .with_extension(format!("new-{}", std::process::id()));
        let write = (|| {
            let mut file = open(&temporary, true)?;
            file.set_len(0)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(&temporary, &self.path)
        })();
        if write.is_err() {
            let _ = std::fs::remove_file(&temporary);
            return Err(failed());
        }
        Ok(result)
    }
    pub fn begin(&self, cards: bool, now: u64, force: bool) -> PluginResult<Begin> {
        self.access(|state| {
            let reading = state.entry(cards);
            if !force && reading.checked_at > 0 && now.saturating_sub(reading.checked_at) < TTL {
                return Ok(Begin::Cached(reading.clone()));
            }
            state.next = state.next.checked_add(1).ok_or_else(failed)?;
            let revision = state.next;
            let reading = state.entry(cards);
            reading.checked_at = now;
            reading.revision = revision;
            Ok(Begin::Read(revision))
        })
    }
    pub fn finish(
        &self,
        cards: bool,
        revision: u64,
        now: u64,
        result: Result<Value, RemoteError>,
    ) -> PluginResult<Reading> {
        self.access(|state| {
            let reading = state.entry(cards);
            if reading.revision != revision {
                return Ok(reading.clone());
            }
            match result {
                Ok(value) => {
                    reading.value = Some(value);
                    reading.error.clear();
                    reading.held = false;
                    reading.read_at = now;
                }
                Err(error) if error.held => {
                    reading.value = None;
                    reading.error.clear();
                    reading.held = true;
                }
                Err(error) => reading.error = error.message,
            }
            Ok(reading.clone())
        })
    }
}
fn open(path: &Path, write: bool) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(write).create(write);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Not a regular cache file",
        ));
    }
    Ok(file)
}
struct Lock(File);
impl Lock {
    fn open(path: &Path) -> PluginResult<Self> {
        let file = open(path, true).map_err(|_| failed())?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
                return Err(failed());
            }
        }
        Ok(Self(file))
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            unsafe {
                libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}
fn failed() -> PluginError {
    PluginError::transport("The remote gateway's saved usage could not be read or updated.")
}
pub(crate) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
#[cfg(test)]
#[path = "../test/cache.test.rs"]
mod tests;
