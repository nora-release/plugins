use fs2::FileExt;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub fn digest(value: &impl serde::Serialize) -> String {
    let mut value = serde_json::to_value(value).unwrap_or(Value::Null);
    value.sort_all_objects();
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&value).unwrap_or_default())
    )
}
pub fn read(path: &Path) -> Option<Value> {
    let file = File::open(path).ok()?;
    if file.metadata().ok()?.len() > 8 * 1024 * 1024 {
        return None;
    }
    serde_json::from_reader(file.take(8 * 1024 * 1024)).ok()
}
pub fn directory(path: &Path) -> PluginResult<()> {
    std::fs::create_dir_all(path)
        .map_err(|_| PluginError::transport("The GCP AI data directory could not be created."))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| PluginError::transport("The GCP AI directory could not be protected."))?;
    }
    Ok(())
}
pub fn write(path: &Path, value: &Value) -> PluginResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| PluginError::invalid("Invalid GCP AI cache path."))?;
    directory(parent)?;
    let temporary = parent.join(format!(".gcp-ai-{}", uuid::Uuid::new_v4()));
    let result = (|| -> std::io::Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&serde_json::to_vec(value)?)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    let _ = std::fs::remove_file(temporary);
    result.map_err(|_| PluginError::transport("The GCP AI cache could not be saved."))
}
pub struct Lock {
    _file: File,
    pub path: PathBuf,
}
impl Lock {
    pub async fn take(path: &Path, timeout: Duration) -> PluginResult<Self> {
        directory(
            path.parent()
                .ok_or_else(|| PluginError::invalid("Invalid GCP AI lock path."))?,
        )?;
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(path.with_extension("lock"))
            .map_err(|_| PluginError::transport("The GCP AI lock could not be opened."))?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => {
                    let _ = std::fs::remove_file(path.with_extension("want"));
                    return Ok(Self {
                        _file: file,
                        path: path.into(),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return Err(PluginError::transport("The GCP AI lock is unavailable.")),
            }
            let _ = std::fs::write(path.with_extension("want"), std::process::id().to_string());
            if tokio::time::Instant::now() >= deadline {
                return Err(PluginError::transport(
                    "Another process is still refreshing GCP AI; retry shortly.",
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    pub fn wanted(&self) -> bool {
        self.path.with_extension("want").is_file()
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::write(
            self.path.with_extension("since"),
            chrono::Utc::now().to_rfc3339(),
        );
    }
}
pub fn timestamp(value: &Value, key: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value.get(key)?.as_str()?)
        .ok()
        .map(|time| time.timestamp_millis())
}

#[cfg(test)]
#[path = "../test/cache.test.rs"]
mod tests;
