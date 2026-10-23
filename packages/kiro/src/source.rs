use super::{
    magpie,
    session::{self, Credential},
    source_kind::{MagpieAccount, SourceKind},
};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

const MAX_SOURCE_BYTES: u64 = 1024 * 1024;

fn conflict() -> PluginError {
    PluginError::authentication(
        "The Kiro sign-in changed or could not be saved. Retry using its current sign-in.",
    )
}

pub(crate) fn lock_root() -> PathBuf {
    #[cfg(unix)]
    let user = unsafe { native::getuid() }.to_string();
    #[cfg(not(unix))]
    let user = session::fingerprint(&std::env::var("USERPROFILE").unwrap_or_default());
    std::env::temp_dir().join(format!("nora-kiro-sources-{user}"))
}

struct SourceLock {
    _file: File,
}

impl SourceLock {
    fn acquire(directory: &Path, kind: &SourceKind, path: &Path) -> PluginResult<Self> {
        if !directory.is_absolute() {
            return Err(conflict());
        }
        if !directory.exists() {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            if let Err(error) = builder.create(directory) {
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(conflict());
                }
            }
        }
        let metadata = std::fs::symlink_metadata(directory).map_err(|_| conflict())?;
        if !metadata.is_dir() {
            return Err(conflict());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.uid() != unsafe { native::getuid() } || metadata.mode() & 0o077 != 0 {
                return Err(conflict());
            }
        }
        let shared_kind = kind.storage_kind();
        let key = session::fingerprint(&format!("{shared_kind}\0{}", path.to_string_lossy()));
        let lock_path = directory.join(format!("{key}.lock"));
        if std::fs::symlink_metadata(&lock_path).is_ok_and(|metadata| !metadata.is_file()) {
            return Err(conflict());
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(lock_path).map_err(|_| conflict())?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if native::try_lock(&file)? {
                break;
            }
            if std::time::Instant::now() >= deadline {
                return Err(conflict());
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        Ok(Self { _file: file })
    }
}

#[cfg(unix)]
mod native {
    use super::*;
    use std::os::fd::AsRawFd;
    extern "C" {
        pub(super) fn getuid() -> u32;
        fn flock(fd: std::ffi::c_int, operation: std::ffi::c_int) -> std::ffi::c_int;
    }
    pub(super) fn try_lock(file: &File) -> PluginResult<bool> {
        loop {
            if unsafe { flock(file.as_raw_fd(), 6) } == 0 {
                return Ok(true);
            }
            match std::io::Error::last_os_error().kind() {
                std::io::ErrorKind::WouldBlock => return Ok(false),
                std::io::ErrorKind::Interrupted => {}
                _ => return Err(conflict()),
            }
        }
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: u32,
        offset_high: u32,
        event: *mut std::ffi::c_void,
    }
    #[link(name = "kernel32")]
    extern "system" {
        #[link_name = "LockFileEx"]
        fn lock_file_ex(
            handle: *mut std::ffi::c_void,
            flags: u32,
            reserved: u32,
            low: u32,
            high: u32,
            overlapped: *mut Overlapped,
        ) -> i32;
    }
    pub(super) fn try_lock(file: &File) -> PluginResult<bool> {
        let mut overlapped = Overlapped {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            event: std::ptr::null_mut(),
        };
        if unsafe {
            lock_file_ex(
                file.as_raw_handle(),
                3,
                0,
                u32::MAX,
                u32::MAX,
                &mut overlapped,
            )
        } == 0
        {
            return if std::io::Error::last_os_error().raw_os_error() == Some(33) {
                Ok(false)
            } else {
                Err(conflict())
            };
        }
        Ok(true)
    }
}

#[derive(Clone, PartialEq)]
struct FileIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(not(unix))]
    created: Option<std::time::SystemTime>,
}
impl FileIdentity {
    fn read(path: &Path) -> PluginResult<Self> {
        let metadata = std::fs::metadata(path).map_err(|_| conflict())?;
        if !metadata.is_file() {
            return Err(conflict());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self {
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {
                created: metadata.created().ok(),
            })
        }
    }
}

#[derive(PartialEq)]
enum Snapshot {
    Cli {
        key: String,
        raw: String,
    },
    Ide(Vec<u8>),
    Magpie {
        account: MagpieAccount,
        entry: Value,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SourceState {
    Original,
    Written,
    Changed,
    SignedOut,
}

pub(crate) struct SourceSession {
    kind: SourceKind,
    path: PathBuf,
    target: PathBuf,
    file: FileIdentity,
    snapshot: Option<Snapshot>,
    credential: Credential,
    expected: Option<Value>,
    lock: Option<SourceLock>,
}

impl SourceSession {
    /// Keep this value alive across source reads, remote renewal and replace.
    /// The lock coordinates Nora processes; source owners are checked again at save.
    pub(crate) fn open(kind: &SourceKind, path: &str, root: &Path) -> PluginResult<Option<Self>> {
        if !Path::new(path).is_absolute() {
            return Err(conflict());
        }
        let path = PathBuf::from(path);
        let target = match std::fs::canonicalize(&path) {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(conflict()),
        };
        let lock = SourceLock::acquire(root, kind, &target)?;
        if std::fs::canonicalize(&path).map_err(|_| conflict())? != target {
            return Err(conflict());
        }
        let file = FileIdentity::read(&target)?;
        let Some((credential, snapshot)) = read(kind, &target)? else {
            return Ok(None);
        };
        if FileIdentity::read(&target)? != file {
            return Err(conflict());
        }
        Ok(Some(Self {
            kind: kind.clone(),
            path,
            target,
            file,
            snapshot: Some(snapshot),
            credential,
            expected: None,
            lock: Some(lock),
        }))
    }
    pub(crate) fn credential(&self) -> &Credential {
        &self.credential
    }

    /// A durable credential no longer needs to exclude other source accounts
    /// while identity services run. The retained snapshot stays read-only.
    pub(crate) fn release_for_verification(&mut self, next: &Credential) -> PluginResult<()> {
        if self.state(next)? != SourceState::Written {
            return Err(conflict());
        }
        self.lock.take();
        Ok(())
    }

    pub(crate) fn state(&self, next: &Credential) -> PluginResult<SourceState> {
        match std::fs::canonicalize(&self.path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SourceState::SignedOut)
            }
            Ok(path) if path != self.target => return Ok(SourceState::Changed),
            Err(_) => return Err(conflict()),
            _ => {}
        }
        let file = FileIdentity::read(&self.target)?;
        let Some((credential, snapshot)) = read(&self.kind, &self.target)? else {
            return Ok(SourceState::SignedOut);
        };
        if FileIdentity::read(&self.target)? != file {
            return Err(conflict());
        }
        if same_binding(&credential, next) && credential.expires == next.expires {
            return Ok(SourceState::Written);
        }
        if (file == self.file || matches!(self.kind, SourceKind::Magpie(_)))
            && self
                .snapshot
                .as_ref()
                .is_some_and(|previous| same_snapshot(previous, &snapshot))
            && same_binding(&credential, &self.credential)
        {
            return Ok(SourceState::Original);
        }
        Ok(SourceState::Changed)
    }

    pub(crate) fn bind_identity(&mut self, saved: &Value) -> PluginResult<()> {
        let identity = saved
            .get("accountIdentity")
            .or_else(|| saved.pointer("/extras/accountIdentity"));
        let strong_user = identity
            .and_then(|identity| identity["subject"].as_str())
            .and_then(|subject| subject.strip_prefix("user:"))
            .filter(|subject| !subject.is_empty());
        if let Some(profile) = session::text(saved.get("profileArn"), 2048) {
            if !self.credential.profile.is_empty() && profile != self.credential.profile {
                return Err(conflict());
            }
        }
        if let Some(value) = saved.get("realm").filter(|_| strong_user.is_none()) {
            let expected = session::realm(value.as_str().ok_or_else(conflict)?)?;
            if expected != self.credential.realm {
                return Err(conflict());
            }
        }
        if let Some(identity) = identity {
            if !self.credential.profile.is_empty() {
                let mut scope = self.credential.clone();
                scope.email = "scope-check@example.invalid".into();
                scope.user_id = strong_user.unwrap_or_default().into();
                let current = scope.account_identity().ok_or_else(conflict)?;
                if current["issuer"] != identity["issuer"] || current["scope"] != identity["scope"]
                {
                    return Err(conflict());
                }
            }
        }
        self.expected = Some(saved.clone());
        Ok(())
    }
    pub(crate) fn reload(&mut self) -> PluginResult<Option<Credential>> {
        self.reload_snapshot(true)
    }
    fn reload_snapshot(&mut self, check_binding: bool) -> PluginResult<Option<Credential>> {
        if std::fs::canonicalize(&self.path).map_err(|_| conflict())? != self.target {
            return Err(conflict());
        }
        let file = FileIdentity::read(&self.target)?;
        let snapshot = read(&self.kind, &self.target)?;
        if FileIdentity::read(&self.target)? != file {
            return Err(conflict());
        }
        self.file = file;
        match snapshot {
            Some((credential, snapshot)) => {
                self.credential = credential.clone();
                self.snapshot = Some(snapshot);
                if let Some(expected) = self.expected.clone().filter(|_| check_binding) {
                    self.bind_identity(&expected)?;
                }
                Ok(Some(credential))
            }
            None => {
                self.snapshot = None;
                Ok(None)
            }
        }
    }
    pub(crate) fn replace(&mut self, next: &Credential) -> PluginResult<()> {
        self.replace_inner(next, false)
    }
    /// Save a server renewal of this exact source snapshot. A server
    /// may move its profile; the caller must separately check its Nora account.
    pub(crate) fn replace_refreshed(&mut self, next: &Credential) -> PluginResult<()> {
        self.replace_inner(next, true)
    }
    fn replace_inner(&mut self, next: &Credential, refreshed: bool) -> PluginResult<()> {
        if self.lock.is_none()
            || !self.compatible(next, refreshed)
            || !refreshed
                && self
                    .expected
                    .as_ref()
                    .is_some_and(|saved| !next.matches_saved_identity(saved))
            || std::fs::canonicalize(&self.path).map_err(|_| conflict())? != self.target
            || !matches!(self.kind, SourceKind::Magpie(_))
                && FileIdentity::read(&self.target)? != self.file
        {
            return Err(conflict());
        }
        match self.snapshot.as_ref().ok_or_else(conflict)? {
            Snapshot::Cli { key, raw } => self.save_cli(key, raw, next)?,
            Snapshot::Ide(bytes) => self.save_ide(bytes, next)?,
            Snapshot::Magpie { account, entry } => self.save_magpie(account, entry, next)?,
        }
        self.reload_snapshot(!refreshed)?.ok_or_else(conflict)?;
        if self.credential.access != next.access || self.credential.refresh != next.refresh {
            return Err(conflict());
        }
        Ok(())
    }
    fn compatible(&self, next: &Credential, refreshed: bool) -> bool {
        let previous = &self.credential;
        previous.method == next.method
            && previous.region == next.region
            && previous.realm == next.realm
            && previous.builder == next.builder
            && previous.client_id == next.client_id
            && previous.client_secret == next.client_secret
            && previous.token_endpoint == next.token_endpoint
            && (refreshed || previous.profile.is_empty() || previous.profile == next.profile)
            && session::secret(Some(&json!(next.access))).is_ok()
            && session::secret(Some(&json!(next.refresh))).is_ok()
            && next.expires > session::now()
    }
    fn save_cli(&self, key: &str, raw: &str, next: &Credential) -> PluginResult<()> {
        let mut db = open_db(&self.target, false)?;
        let transaction = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| conflict())?;
        let (current, current_key, current_raw) =
            session::cli_snapshot(&transaction)?.ok_or_else(conflict)?;
        if current_key != key
            || current_raw != raw
            || !same_binding(&current, &self.credential)
            || FileIdentity::read(&self.target)? != self.file
        {
            return Err(conflict());
        }
        let mut value: Value = serde_json::from_str(raw).map_err(|_| conflict())?;
        value["access_token"] = json!(next.access);
        value["refresh_token"] = json!(next.refresh);
        value["expires_at"] = json!(expiry(next.expires)?);
        if !next.profile.is_empty() {
            value["profile_arn"] = json!(next.profile);
        }
        let written = transaction
            .execute(
                "UPDATE auth_kv SET value=?1 WHERE key=?2 AND value=?3",
                [&value.to_string(), key, raw],
            )
            .map_err(|_| conflict())?;
        if written != 1 {
            return Err(conflict());
        }
        transaction.commit().map_err(|_| conflict())?;
        if FileIdentity::read(&self.target)? != self.file {
            return Err(conflict());
        }
        Ok(())
    }
    fn save_ide(&self, bytes: &[u8], next: &Credential) -> PluginResult<()> {
        let mut value: Value = serde_json::from_slice(bytes).map_err(|_| conflict())?;
        value["accessToken"] = json!(next.access);
        value["refreshToken"] = json!(next.refresh);
        value["expiresAt"] = json!(expiry(next.expires)?);
        if !next.profile.is_empty() {
            value["profileArn"] = json!(next.profile);
        }
        self.save_json(bytes, &self.file, &value, || true)
    }
    fn save_magpie(
        &self,
        account: &MagpieAccount,
        entry: &Value,
        next: &Credential,
    ) -> PluginResult<()> {
        let lock = magpie::WriteLock::acquire(&self.path)?;
        let file = FileIdentity::read(&self.target)?;
        let bytes = read_bytes_limited(&self.target, magpie::MAX_BYTES)?;
        let mut value = magpie::document(&bytes)?;
        let current = magpie::credential(account, &value)?.ok_or_else(conflict)?;
        if !value
            .get(account.as_str())
            .is_some_and(|current| magpie::same_credentials(entry, current))
            || !same_binding(&current, &self.credential)
        {
            return Err(conflict());
        }
        let target = value
            .get_mut(account.as_str())
            .and_then(Value::as_object_mut)
            .ok_or_else(conflict)?;
        target.insert("access".into(), json!(next.access));
        target.insert("refresh".into(), json!(next.refresh));
        target.insert("expires".into(), json!(next.expires));
        if !next.profile.is_empty() {
            target.insert("profileArn".into(), json!(next.profile));
        }
        self.save_json(&bytes, &file, &value, || lock.still_owned())
    }
    fn save_json(
        &self,
        bytes: &[u8],
        expected_file: &FileIdentity,
        value: &Value,
        still_locked: impl Fn() -> bool,
    ) -> PluginResult<()> {
        let directory = self.target.parent().ok_or_else(conflict)?;
        let path = directory.join(format!(
            ".nora-kiro-{}.tmp",
            nora_ai::oauth_pkce::random_hex(16)
        ));
        let temporary = Temporary(path);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary.0).map_err(|_| conflict())?;
        file.write_all(&serde_json::to_vec(&value).map_err(|_| conflict())?)
            .map_err(|_| conflict())?;
        file.sync_all().map_err(|_| conflict())?;
        let is_magpie = matches!(self.kind, SourceKind::Magpie(_));
        let current = read_bytes_limited(
            &self.target,
            if is_magpie {
                magpie::MAX_BYTES
            } else {
                MAX_SOURCE_BYTES
            },
        )?;
        let document = serde_json::from_slice::<Value>(&current).map_err(|_| conflict())?;
        let credential = match &self.kind {
            SourceKind::Magpie(account) => magpie::credential(account, &document)?,
            SourceKind::Ide => session::ide_credential(&document, &self.target)?,
            SourceKind::Cli => return Err(conflict()),
        }
        .ok_or_else(conflict)?;
        if current != bytes
            || !same_binding(&credential, &self.credential)
            || &FileIdentity::read(&self.target)? != expected_file
            || std::fs::canonicalize(&self.path).map_err(|_| conflict())? != self.target
            || !still_locked()
        {
            return Err(conflict());
        }
        std::fs::rename(&temporary.0, &self.target).map_err(|_| conflict())?;
        #[cfg(unix)]
        File::open(directory)
            .and_then(|file| file.sync_all())
            .map_err(|_| conflict())?;
        Ok(())
    }
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn expiry(value: i64) -> PluginResult<String> {
    chrono::DateTime::from_timestamp_millis(value)
        .map(|date| date.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .ok_or_else(conflict)
}
fn same_binding(left: &Credential, right: &Credential) -> bool {
    left.access == right.access
        && left.refresh == right.refresh
        && left.method == right.method
        && left.region == right.region
        && left.profile == right.profile
        && left.realm == right.realm
        && left.builder == right.builder
        && left.client_id == right.client_id
        && left.client_secret == right.client_secret
        && left.token_endpoint == right.token_endpoint
}
fn same_snapshot(left: &Snapshot, right: &Snapshot) -> bool {
    match (left, right) {
        (
            Snapshot::Magpie {
                account: a,
                entry: left,
            },
            Snapshot::Magpie {
                account: b,
                entry: right,
            },
        ) => a == b && magpie::same_credentials(left, right),
        _ => left == right,
    }
}
fn open_db(path: &Path, readonly: bool) -> PluginResult<rusqlite::Connection> {
    let flags = if readonly {
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    let db = rusqlite::Connection::open_with_flags(
        path,
        flags | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| conflict())?;
    db.busy_timeout(std::time::Duration::from_secs(3))
        .map_err(|_| conflict())?;
    Ok(db)
}
fn read(kind: &SourceKind, path: &Path) -> PluginResult<Option<(Credential, Snapshot)>> {
    if *kind == SourceKind::Cli {
        let mut db = open_db(path, true)?;
        let transaction = db.transaction().map_err(|_| conflict())?;
        return session::cli_snapshot(&transaction).map(|snapshot| {
            snapshot.map(|(credential, key, raw)| (credential, Snapshot::Cli { key, raw }))
        });
    }
    if let SourceKind::Magpie(account) = kind {
        let bytes = read_bytes_limited(path, magpie::MAX_BYTES)?;
        let document = magpie::document(&bytes)?;
        return magpie::credential(account, &document).map(|credential| {
            credential.map(|credential| {
                (
                    credential,
                    Snapshot::Magpie {
                        account: account.clone(),
                        entry: document[account.as_str()].clone(),
                    },
                )
            })
        });
    }
    let bytes = read_bytes(path)?;
    let value = serde_json::from_slice::<Value>(&bytes).map_err(|_| conflict())?;
    session::ide_credential(&value, path)
        .map(|credential| credential.map(|credential| (credential, Snapshot::Ide(bytes))))
}
fn read_bytes(path: &Path) -> PluginResult<Vec<u8>> {
    read_bytes_limited(path, MAX_SOURCE_BYTES)
}
fn read_bytes_limited(path: &Path, limit: u64) -> PluginResult<Vec<u8>> {
    let file = File::open(path).map_err(|_| conflict())?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| conflict())?;
    if bytes.len() as u64 > limit {
        return Err(conflict());
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "../test/source.test.rs"]
mod tests;
