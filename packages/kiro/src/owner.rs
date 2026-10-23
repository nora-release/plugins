use super::*;
use std::{path::Path, process::Stdio, time::Duration};

#[async_trait]
pub(super) trait CliOwner: Send + Sync {
    fn owns(&self, path: &Path) -> bool;
    async fn renew(&self, proxy: Option<&NetworkProxy>) -> PluginResult<()>;
}

pub(super) struct InstalledCli;

#[async_trait]
impl CliOwner for InstalledCli {
    fn owns(&self, path: &Path) -> bool {
        discovery::default_cli_path().is_some_and(|default| {
            std::fs::canonicalize(default)
                .ok()
                .zip(std::fs::canonicalize(path).ok())
                .is_some_and(|(default, selected)| default == selected)
        })
    }

    async fn renew(&self, proxy: Option<&NetworkProxy>) -> PluginResult<()> {
        if let Some(proxy) = proxy {
            proxy.validate()?;
        }
        let executable = executable().ok_or_else(unavailable)?;
        let mut command = tokio::process::Command::new(executable);
        command
            .args(["debug", "refresh-auth-token"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if proxy.is_some() {
            for name in [
                "HTTP_PROXY",
                "HTTPS_PROXY",
                "ALL_PROXY",
                "NO_PROXY",
                "http_proxy",
                "https_proxy",
                "all_proxy",
                "no_proxy",
            ] {
                command.env_remove(name);
            }
        }
        match proxy {
            Some(NetworkProxy::Proxy { url }) => {
                command
                    .env("HTTPS_PROXY", url)
                    .env("HTTP_PROXY", url)
                    .env("ALL_PROXY", url);
            }
            Some(NetworkProxy::Direct) => {
                command.env("NO_PROXY", "*");
            }
            None => {}
        }
        let status = tokio::time::timeout(Duration::from_secs(20), command.status())
            .await
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())?;
        if !status.success() {
            return Err(unavailable());
        }
        Ok(())
    }
}

fn unavailable() -> PluginError {
    PluginError::transport("The Kiro CLI could not renew its sign-in.")
}

fn executable() -> Option<std::path::PathBuf> {
    let name = if cfg!(windows) {
        "kiro-cli.exe"
    } else {
        "kiro-cli"
    };
    let mut directories = std::env::var_os("PATH")
        .map(|value| {
            std::env::split_paths(&value)
                .filter(|path| path.is_absolute())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME") {
        directories.push(std::path::PathBuf::from(home).join(".local/bin"));
    }
    for directory in [
        "/Applications/Kiro CLI.app/Contents/MacOS",
        "/usr/local/bin",
        "/opt/homebrew/bin",
    ] {
        directories.push(directory.into());
    }
    directories
        .into_iter()
        .map(|directory| directory.join(name))
        .find(|path| {
            let Ok(metadata) = std::fs::metadata(path) else {
                return false;
            };
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            {
                metadata.is_file()
            }
        })
}
