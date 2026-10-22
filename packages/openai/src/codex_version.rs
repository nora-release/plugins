use serde::Deserialize;
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};
use tokio::io::AsyncReadExt;

pub(crate) const MINIMUM: &str = "0.159.2";
const CHECK_EVERY: Duration = Duration::from_secs(10 * 60);
const MAX_CACHE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Default)]
struct Cache {
    checked_at: Option<Instant>,
    version: String,
    failed_binaries: BTreeSet<BinaryStamp>,
}
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct BinaryStamp {
    path: PathBuf,
    size: u64,
    modified: Option<SystemTime>,
}

pub(crate) fn models_cache_path() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| home().map(|path| path.join(".codex")))
        .map(|path| path.join("models_cache.json"))
}

pub(crate) async fn current() -> String {
    static CACHE: OnceLock<tokio::sync::Mutex<Cache>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().await;
    if cache
        .checked_at
        .is_some_and(|at| at.elapsed() < CHECK_EVERY)
    {
        return cache.version.clone();
    }
    let cached = models_cache_path().and_then(|path| cache_version(&path));
    let installed = installed_version(&mut cache.failed_binaries).await;
    cache.version = newest([Some(MINIMUM), cached.as_deref(), installed.as_deref()]);
    cache.checked_at = Some(Instant::now());
    cache.version.clone()
}

pub(crate) fn headers(version: &str) -> std::collections::BTreeMap<String, String> {
    std::collections::BTreeMap::from([
        ("originator".into(), "codex_cli_rs".into()),
        ("version".into(), version.into()),
        (
            "user-agent".into(),
            format!(
                "codex_cli_rs/{version} ({}; {}) Nora",
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
        ),
    ])
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn cache_version(path: &Path) -> Option<String> {
    #[derive(Deserialize)]
    struct Metadata {
        client_version: Option<String>,
    }
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_CACHE_BYTES {
        return None;
    }
    let metadata: Metadata = serde_json::from_reader(file.take(MAX_CACHE_BYTES)).ok()?;
    metadata
        .client_version
        .and_then(|version| normalized(&version))
}

async fn installed_version(failed: &mut BTreeSet<BinaryStamp>) -> Option<String> {
    for candidate in executable_candidates() {
        let Ok(real) = candidate.canonicalize() else {
            continue;
        };
        let Ok(metadata) = real.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        if let Some(version) = npm_version(&real) {
            return Some(version);
        }
        if !trusted_native_path(&real) || !executable(&metadata) {
            continue;
        }
        let stamp = BinaryStamp {
            path: real.clone(),
            size: metadata.len(),
            modified: metadata.modified().ok(),
        };
        if failed.contains(&stamp) {
            continue;
        }
        match native_version(&real).await {
            Ok(Some(version)) => return Some(version),
            Err(()) => {} // A timeout is retried on the next scheduled check.
            Ok(None) => {
                failed.insert(stamp);
            }
        }
    }
    None
}

fn executable_candidates() -> Vec<PathBuf> {
    let mut paths = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .filter(|path| path.is_absolute())
        .map(|path| path.join("codex"))
        .collect::<Vec<_>>();
    paths.extend(
        [
            "/opt/homebrew/bin/codex",
            "/usr/local/bin/codex",
            "/Applications/Codex.app/Contents/Resources/codex",
        ]
        .map(PathBuf::from),
    );
    if let Some(home) = home() {
        paths.extend(
            [
                ".local/bin/codex",
                ".npm-global/bin/codex",
                ".volta/bin/codex",
                "Applications/Codex.app/Contents/Resources/codex",
            ]
            .map(|path| home.join(path)),
        );
        for directory in [".nvm/versions/node", ".local/share/mise/installs/node"] {
            if let Ok(versions) = std::fs::read_dir(home.join(directory)) {
                paths.extend(
                    versions
                        .filter_map(Result::ok)
                        .map(|entry| entry.path().join("bin/codex")),
                );
            }
        }
    }
    let mut seen = BTreeSet::new();
    paths.retain(|path| seen.insert(path.clone()));
    paths
}

fn npm_version(executable: &Path) -> Option<String> {
    #[derive(Deserialize)]
    struct Package {
        name: String,
        version: String,
    }
    for directory in executable.ancestors().skip(1).take(12) {
        let path = directory.join("package.json");
        let Ok(file) = std::fs::File::open(path) else {
            continue;
        };
        if file.metadata().ok()?.len() > 1024 * 1024 {
            continue;
        }
        let Ok(package) = serde_json::from_reader::<_, Package>(file.take(1024 * 1024)) else {
            continue;
        };
        if package.name == "@openai/codex" || package.name.starts_with("@openai/codex-") {
            return normalized(&package.version);
        }
    }
    None
}

fn trusted_native_path(path: &Path) -> bool {
    let system = [
        "/opt/homebrew/bin/codex",
        "/usr/local/bin/codex",
        "/opt/homebrew/Cellar/codex",
        "/usr/local/Cellar/codex",
        "/opt/homebrew/Caskroom/codex",
        "/usr/local/Caskroom/codex",
        "/Applications/Codex.app/Contents/Resources",
    ];
    if system.iter().any(|root| path.starts_with(root)) {
        return true;
    }
    home().is_some_and(|home| {
        [
            ".local/bin/codex",
            ".local/share/codex",
            "Applications/Codex.app/Contents/Resources",
        ]
        .iter()
        .any(|root| path.starts_with(home.join(root)))
    })
}

#[cfg(unix)]
fn executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}
#[cfg(not(unix))]
fn executable(_: &std::fs::Metadata) -> bool {
    true
}

async fn native_version(path: &Path) -> Result<Option<String>, ()> {
    let mut command = tokio::process::Command::new(path);
    command
        .arg("--version")
        .env_clear()
        .env(
            "PATH",
            "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin",
        )
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(home) = home() {
        command.current_dir(&home).env("HOME", home);
    }
    let Ok(mut child) = command.spawn() else {
        return Ok(None);
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut output = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            if stdout.take(4096).read_to_end(&mut output).await.is_err() {
                return None;
            }
        }
        let _ = child.wait().await;
        normalized(&String::from_utf8_lossy(&output))
    })
    .await
    .map_err(|_| ())
}

fn newest<'a>(versions: impl IntoIterator<Item = Option<&'a str>>) -> String {
    versions
        .into_iter()
        .flatten()
        .filter_map(|version| {
            normalized(version).and_then(|text| semver(&text).map(|number| (number, text)))
        })
        .max_by(|left, right| left.0.cmp(&right.0))
        .map(|(_, text)| text)
        .unwrap_or_else(|| MINIMUM.into())
}

fn normalized(output: &str) -> Option<String> {
    for (start, character) in output.char_indices() {
        if !character.is_ascii_digit() {
            continue;
        }
        let candidate = output[start..]
            .split(|character: char| !character.is_ascii_digit() && character != '.')
            .next()?;
        let parts = candidate.split('.').collect::<Vec<_>>();
        if parts.len() >= 3 {
            let candidate = parts[..3].join(".");
            if semver(&candidate).is_some() {
                return Some(candidate);
            }
        }
    }
    None
}
fn semver(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.');
    let value = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn floor_cache_and_installed_cli_are_compared_numerically() {
        assert_eq!(
            newest([Some(MINIMUM), Some("0.160.0"), Some("codex-cli 0.161.1")]),
            "0.161.1"
        );
        assert_eq!(newest([Some(MINIMUM), Some("0.160.2"), None]), "0.160.2");
        assert_eq!(
            newest([Some(MINIMUM), Some("0.99.20"), Some("not a version")]),
            MINIMUM
        );
        assert_eq!(newest([Some("0.9.99"), Some("0.10.0")]), "0.10.0");
        assert_eq!(newest([Some("0.160.9"), Some("0.160.10")]), "0.160.10");
    }
    #[test]
    fn version_output_and_prerelease_suffixes_match_magpie_version_parsing() {
        assert_eq!(
            normalized("codex-cli 0.160.0-alpha.3"),
            Some("0.160.0".into())
        );
        assert_eq!(
            normalized("\u{1b}[32mcodex-cli 0.161.2\u{1b}[0m"),
            Some("0.161.2".into())
        );
        for value in ["", "0.159", "vlatest", "1.two.3"] {
            assert_eq!(normalized(value), None);
        }
    }
    #[test]
    fn project_or_temporary_codex_programs_are_not_executed() {
        for path in [
            "/tmp/codex",
            "/some/project/node_modules/.bin/codex",
            "./codex",
            "/usr/local/bin/codex-other",
        ] {
            assert!(!trusted_native_path(Path::new(path)));
        }
        assert!(trusted_native_path(Path::new(
            "/Applications/Codex.app/Contents/Resources/codex"
        )));
        assert!(trusted_native_path(Path::new(
            "/opt/homebrew/Cellar/codex/0.160.0/bin/codex"
        )));
    }

    #[test]
    fn installed_npm_package_and_cli_cache_are_read_without_running_programs() {
        let root = std::env::temp_dir().join(format!("nora-codex-version-{}", std::process::id()));
        let package = root.join("node_modules/@openai/codex");
        std::fs::create_dir_all(package.join("bin")).unwrap();
        std::fs::write(
            package.join("package.json"),
            br#"{"name":"@openai/codex","version":"0.161.1"}"#,
        )
        .unwrap();
        std::fs::write(package.join("bin/codex.js"), b"This fixture must never run").unwrap();
        let cache = root.join("models_cache.json");
        std::fs::write(&cache, br#"{"client_version":"0.160.2","models":[]}"#).unwrap();
        let cached = cache_version(&cache).unwrap();
        let installed = npm_version(&package.join("bin/codex.js")).unwrap();
        assert_eq!(
            newest([Some(MINIMUM), Some(&cached), Some(&installed)]),
            "0.161.1"
        );
        std::fs::write(&cache, b"malformed").unwrap();
        assert!(cache_version(&cache).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
