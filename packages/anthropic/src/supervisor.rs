//! The lifetime socket is owned by the plugin, not by Claude or its MCP helpers.
//! Its EOF still reaches the supervisor when the plugin is killed without Drop.
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};

#[cfg(unix)]
pub(crate) struct Lifetime {
    read: Option<std::os::unix::net::UnixStream>,
    _write: std::os::unix::net::UnixStream,
}
#[cfg(not(unix))]
pub(crate) struct Lifetime;

#[cfg(unix)]
pub(crate) fn configure(
    command: &mut tokio::process::Command,
    executable: &std::path::Path,
    args: &[String],
) -> std::io::Result<Lifetime> {
    use std::os::unix::process::CommandExt;
    let (read, write) = std::os::unix::net::UnixStream::pair()?;
    let fd = read.as_raw_fd();
    command
        .arg("claude-supervisor")
        .arg(fd.to_string())
        .arg(executable)
        .args(args);
    command.as_std_mut().process_group(0);
    // Only the spawned supervisor inherits this endpoint. The plugin keeps
    // the write endpoint CLOEXEC so another Claude process cannot retain it.
    unsafe {
        command.as_std_mut().pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    // Command::spawn executes pre_exec synchronously, so this endpoint must
    // remain alive until spawn returns. A held endpoint is returned separately.
    Ok(Lifetime {
        read: Some(read),
        _write: write,
    })
}

#[cfg(unix)]
impl Lifetime {
    pub(crate) fn release_read(&mut self) {
        self.read.take();
    }
}

pub(crate) fn executable() -> std::io::Result<std::path::PathBuf> {
    let path = std::env::current_exe()?;
    #[cfg(test)]
    {
        let binary = path
            .parent()
            .and_then(std::path::Path::parent)
            .ok_or_else(|| std::io::Error::other("The provider test executable is unavailable."))?
            .join(format!(
                "nora-provider-anthropic{}",
                std::env::consts::EXE_SUFFIX
            ));
        return Ok(binary);
    }
    #[cfg(not(test))]
    Ok(path)
}

pub(crate) fn run(args: &[String]) -> i32 {
    #[cfg(unix)]
    {
        use std::io::Read;
        if args.len() < 2 {
            return 1;
        }
        let Some(fd) = args[0].parse::<i32>().ok().filter(|fd| *fd >= 3) else {
            return 1;
        };
        let group = unsafe { libc::getpid() };
        // Never signal a group created by a shell or another application.
        if unsafe { libc::getpgrp() } != group {
            return 1;
        }
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
            return 1;
        }
        let mut lifeline = unsafe { std::fs::File::from_raw_fd(fd) };
        if std::thread::Builder::new()
            .name("claude-lifetime".into())
            .spawn(move || {
                let mut byte = [0u8; 1];
                loop {
                    match lifeline.read(&mut byte) {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
                unsafe {
                    libc::kill(-group, libc::SIGKILL);
                }
            })
            .is_err()
        {
            return 1;
        }
        // Claude and its MCP children inherit this supervisor's private group.
        // All three stdio descriptors are inherited without a buffering layer.
        let mut child = match std::process::Command::new(&args[1])
            .args(&args[2..])
            .spawn()
        {
            Ok(child) => child,
            Err(_) => return 1,
        };
        return child
            .wait()
            .ok()
            .and_then(|status| status.code())
            .unwrap_or(1);
    }
    #[cfg(not(unix))]
    {
        let _ = args;
        1
    }
}
