use super::cli;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use std::path::{Path, PathBuf};

pub(crate) fn owner(account: &cli::Account) -> String {
    cli::hash(&format!(
        "{}\0{}\0{}",
        account.identity,
        account.home.directory().display(),
        account.executable.display()
    ))
}

pub(crate) fn work_directory(account: &cli::Account, root: &Path) -> PluginResult<PathBuf> {
    let name = cli::hash(&format!("{}\0{}", owner(account), root.display()));
    let directory = std::env::temp_dir().join(format!("nora-claude-work-{}", &name[..40]));
    match std::fs::create_dir(&directory) {
        Ok(()) => cli::directory(&directory)?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(unavailable()),
    }
    if !trusted(&directory, true) {
        return Err(unavailable());
    }
    directory.canonicalize().map_err(|_| unavailable())
}

fn unavailable() -> PluginError {
    PluginError::transport("The private Claude working directory is unavailable.")
}

fn trusted(path: &Path, directory: bool) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if metadata.is_dir() != directory || (!directory && !metadata.is_file()) {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o022 == 0
    }
    #[cfg(not(unix))]
    true
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

pub(crate) fn project_name(directory: &Path) -> String {
    directory
        .to_string_lossy()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect()
}

/// Only sessions reported by this run's CLI in its account's private project
/// directory may be retained. Taking this value transfers file cleanup too.
pub(crate) struct Session {
    pub id: String,
    project: PathBuf,
}
impl Session {
    pub fn capture(account: &cli::Account, work: &Path, id: &str) -> PluginResult<Self> {
        if !valid_id(id) {
            return Err(PluginError::transport(
                "Claude Code returned an invalid session ID.",
            ));
        }
        let home = account
            .home
            .directory()
            .canonicalize()
            .map_err(|_| unavailable())?;
        Ok(Self {
            id: id.into(),
            project: home.join("projects").join(project_name(work)),
        })
    }
    pub fn exists(&self) -> bool {
        trusted(&self.project, true)
            && trusted(&self.project.join(format!("{}.jsonl", self.id)), false)
            && self
                .project
                .canonicalize()
                .is_ok_and(|path| path == self.project)
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if self
            .project
            .canonicalize()
            .is_ok_and(|path| path == self.project)
        {
            let _ = std::fs::remove_file(self.project.join(format!("{}.jsonl", self.id)));
            let extra = self.project.join(&self.id);
            if trusted(&extra, true) {
                let _ = std::fs::remove_dir_all(extra);
            }
        }
    }
}

#[cfg(test)]
#[path = "../test/cli_sessions.test.rs"]
mod tests;
