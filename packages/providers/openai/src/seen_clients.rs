use super::{newest, normalized, semver};
use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const VERSION_FILE: &str = "codex-client-version";
const MAX_VERSION_BYTES: u64 = 256;

/// A metadata worker must see versions learned by a separate fetch worker.
#[derive(Default)]
pub(super) struct SeenClients {
    directory: Option<PathBuf>,
    version: Option<String>,
}

impl SeenClients {
    pub(super) fn configure(&mut self, directory: &Path) {
        if self.directory.as_deref() != Some(directory) {
            self.directory = Some(directory.to_owned());
            self.version = None;
        }
    }

    pub(super) fn get(&mut self) -> Option<String> {
        let saved = self
            .directory
            .as_ref()
            .and_then(|directory| read_version(&directory.join(VERSION_FILE)));
        self.version = latest(self.version.as_deref(), saved.as_deref());
        self.version.clone()
    }

    pub(super) fn observe(
        &mut self,
        user_agent: Option<&str>,
        version: Option<&str>,
        originator: Option<&str>,
    ) {
        let Some(version) = client_version(user_agent, version, originator) else {
            return;
        };
        self.version = latest(self.version.as_deref(), Some(&version));
        if let Some(directory) = &self.directory {
            if let Ok(saved) = save_version(directory, self.version.as_deref().unwrap()) {
                self.version = Some(saved);
            }
        }
    }
}

fn latest(left: Option<&str>, right: Option<&str>) -> Option<String> {
    (left.is_some() || right.is_some()).then(|| newest([left, right]))
}

fn client_version(
    user_agent: Option<&str>,
    version: Option<&str>,
    originator: Option<&str>,
) -> Option<String> {
    let user_agent = user_agent.unwrap_or_default();
    if originator.unwrap_or_default().is_empty()
        && !user_agent.to_ascii_lowercase().starts_with("codex")
    {
        return None;
    }
    if let Some(version) = version.and_then(normalized) {
        return Some(version);
    }
    let (_, rest) = user_agent.split_once('/')?;
    let leading = rest
        .split(|character: char| !character.is_ascii_digit() && character != '.')
        .next()?;
    let parts = leading.split('.').take(3).collect::<Vec<_>>();
    if parts.len() != 3 {
        return None;
    }
    let candidate = parts.join(".");
    semver(&candidate).map(|_| candidate)
}

fn read_version(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_VERSION_BYTES {
        return None;
    }
    let mut version = String::new();
    file.take(MAX_VERSION_BYTES)
        .read_to_string(&mut version)
        .ok()?;
    normalized(&version)
}

fn save_version(directory: &Path, version: &str) -> std::io::Result<String> {
    std::fs::create_dir_all(directory)?;
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(directory.join("codex-client-version.lock"))?;
    lock.lock_exclusive()?;
    let path = directory.join(VERSION_FILE);
    let saved = read_version(&path);
    let version = newest([Some(version), saved.as_deref()]);
    if saved.as_deref() == Some(&version) {
        return Ok(version);
    }
    let temporary = directory.join(format!(".codex-client-version-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(version.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temporary, &path)?;
        File::open(directory)?.sync_all()?;
        Ok(version)
    })();
    let _ = std::fs::remove_file(temporary);
    result
}

#[cfg(test)]
#[path = "../test/seen_clients.test.rs"]
mod tests;
