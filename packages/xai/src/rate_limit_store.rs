use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

pub(super) fn directory(root: &Path) -> io::Result<PathBuf> {
    if !root.is_absolute() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let directory = root.join("rate-limits");
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&directory)?;
    if !fs::symlink_metadata(&directory)?.is_dir() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(directory)
}

struct Account {
    _lock: File,
    state: PathBuf,
}
impl Account {
    fn open(directory: &Path, key: &str) -> io::Result<Self> {
        let key: String = Sha256::digest(key.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(directory.join(format!("{key}.lock")))?;
        fs2::FileExt::lock_exclusive(&lock)?;
        Ok(Self {
            _lock: lock,
            state: directory.join(format!("{key}.until")),
        })
    }

    fn clear(&self) -> io::Result<()> {
        match fs::remove_file(&self.state) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn write(&self, until: i64) -> io::Result<()> {
        let temporary = self
            .state
            .with_extension(format!("{}.tmp", nora_ai::oauth_pkce::random_hex(8)));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(until.to_string().as_bytes())?;
            drop(file);
            #[cfg(windows)]
            self.clear()?;
            fs::rename(&temporary, &self.state)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}

pub(super) fn update(directory: &Path, key: &str, until: Option<i64>) -> io::Result<()> {
    let account = Account::open(directory, key)?;
    match until {
        Some(until) => account.write(until),
        None => account.clear(),
    }
}

pub(super) fn read(directory: &Path, key: &str, now: i64) -> io::Result<Option<i64>> {
    let account = Account::open(directory, key)?;
    let file = match File::open(&account.state) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut value = String::new();
    file.take(64).read_to_string(&mut value)?;
    let until = value.parse::<i64>().ok().filter(|until| *until > now);
    if until.is_none() {
        account.clear()?;
    }
    Ok(until)
}
