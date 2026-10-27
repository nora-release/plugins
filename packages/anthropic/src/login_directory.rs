use super::cli;
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

/// Only a newly created login directory is owned here. A successful sign-in
/// releases it to its saved account; all other exits remove its credentials.
pub(super) struct Directory {
    pub path: PathBuf,
    released: AtomicBool,
}

impl Directory {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            released: AtomicBool::new(false),
        }
    }

    pub fn keep(&self) {
        self.released.store(true, Ordering::Release);
    }

    pub fn clear(&self) {
        self.clear_with(
            if cfg!(target_os = "macos") {
                Some(Path::new("security"))
            } else {
                None
            },
            Duration::from_millis(750),
        );
    }

    fn clear_with(&self, security: Option<&Path>, timeout: Duration) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(security) = security {
            let service = cli::Home::Isolated(self.path.clone()).keychain_service();
            if let Ok(mut child) = Command::new(security)
                .args(["delete-generic-password", "-s", &service])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                // The host terminates the private plugin after cancel returns.
                // Finish this bounded cleanup before that acknowledgement.
                let deadline = Instant::now() + timeout;
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) => break,
                        Ok(None) if Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(10))
                        }
                        _ => {
                            let _ = child.kill();
                            let _ = child.wait();
                            break;
                        }
                    }
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
#[path = "../test/login-directory.test.rs"]
mod tests;
