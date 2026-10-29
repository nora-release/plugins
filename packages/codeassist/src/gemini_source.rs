use super::*;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub(crate) const METHOD: &str = "geminiCLI";
const MAX_FILE: u64 = 1024 * 1024;

pub(crate) struct Snapshot {
    pub tokens: session::Tokens,
    pub path: PathBuf,
    original: Vec<u8>,
    pub key: String,
    target: PathBuf,
    epoch: Epoch,
}

#[derive(PartialEq)]
struct Epoch {
    modified: Option<std::time::SystemTime>,
    size: u64,
    #[cfg(unix)]
    identity: (u64, u64),
}
impl Epoch {
    fn read(path: &Path) -> PluginResult<Self> {
        let metadata = std::fs::metadata(path).map_err(|_| expired())?;
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            modified: metadata.modified().ok(),
            size: metadata.len(),
            #[cfg(unix)]
            identity: (metadata.dev(), metadata.ino()),
        })
    }
}

pub(crate) fn default_path() -> PluginResult<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .ok_or_else(expired)?;
    Ok(home.join(".gemini/oauth_creds.json"))
}

pub(crate) fn bound(value: &Value) -> bool {
    value.pointer("/source/kind").and_then(Value::as_str) == Some(METHOD)
}

fn bytes(path: &Path) -> PluginResult<Vec<u8>> {
    if !path.is_absolute() {
        return Err(expired());
    }
    let file = std::fs::File::open(path).map_err(|_| expired())?;
    if !file.metadata().map_err(|_| expired())?.is_file() {
        return Err(expired());
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| expired())?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(expired());
    }
    Ok(bytes)
}

fn json_file(path: &Path) -> PluginResult<Value> {
    serde_json::from_slice(&bytes(path)?).map_err(|_| expired())
}

impl Snapshot {
    pub(crate) fn ensure_current(&self) -> PluginResult<()> {
        if std::fs::canonicalize(&self.path).map_err(|_| expired())? != self.target
            || Epoch::read(&self.target)? != self.epoch
            || bytes(&self.target)? != self.original
        {
            return Err(expired());
        }
        Ok(())
    }
    pub(crate) fn read(path: &Path) -> PluginResult<Self> {
        if !path.is_absolute() {
            return Err(expired());
        }
        let target = std::fs::canonicalize(path).map_err(|_| expired())?;
        let epoch = Epoch::read(&target)?;
        let original = bytes(&target)?;
        let mut tokens = session::selected(target.to_str().ok_or_else(expired)?, App::GeminiCLI)?;
        if Epoch::read(&target)? != epoch || bytes(&target)? != original {
            return Err(expired());
        }
        let directory = path.parent().ok_or_else(expired)?;
        let active = json_file(&directory.join("google_accounts.json"))
            .ok()
            .and_then(|value| text(value.get("active"), 512).map(str::to_owned))
            .unwrap_or_default();
        tokens.email = active.clone();
        if let Ok(contents) = bytes(&directory.join(".env"))
            .and_then(|bytes| String::from_utf8(bytes).map_err(|_| expired()))
        {
            tokens.project = project_from_env(&contents).unwrap_or(tokens.project);
        }
        let key = fingerprint(&format!(
            "{}\0{}\0{}\0{}",
            path.display(),
            fingerprint(&String::from_utf8_lossy(&original)),
            active,
            tokens.project
        ));
        Ok(Self {
            tokens,
            path: path.to_path_buf(),
            original,
            key,
            target,
            epoch,
        })
    }

    pub(crate) fn write_renewed(&self, tokens: &session::Tokens) -> PluginResult<()> {
        // A concurrent Gemini login or refresh owns its newer file. Never
        // replace it with the credentials read before our network request.
        self.ensure_current()?;
        let mut value: Value = serde_json::from_slice(&self.original).map_err(|_| expired())?;
        let object = value.as_object_mut().ok_or_else(expired)?;
        object.insert("access_token".into(), json!(tokens.access));
        object.insert("refresh_token".into(), json!(tokens.refresh));
        object.insert("expiry_date".into(), json!(tokens.expires));
        let bytes = serde_json::to_vec_pretty(&value).map_err(|_| expired())?;
        let directory = self.target.parent().ok_or_else(expired)?;
        let staged = directory.join(format!(
            ".oauth_creds.nora-{}",
            nora_ai::oauth_pkce::random_hex(12)
        ));
        let result = (|| -> PluginResult<()> {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&staged).map_err(|_| expired())?;
            file.write_all(&bytes).map_err(|_| expired())?;
            file.sync_all().map_err(|_| expired())?;
            self.ensure_current()?;
            std::fs::rename(&staged, &self.target).map_err(|_| expired())?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&staged);
        }
        result
    }
}

pub(crate) fn saved(tokens: &session::Tokens, path: &Path) -> PluginResult<Value> {
    let path = path.to_str().ok_or_else(expired)?;
    let mut value = session::saved(tokens, App::GeminiCLI, Some(path));
    value["authMethod"] = json!(METHOD);
    value["source"]["kind"] = json!(METHOD);
    value["extras"]["noraAccountSource"]["kind"] = json!(METHOD);
    Ok(value)
}

fn project_from_env(contents: &str) -> Option<String> {
    contents
        .lines()
        .filter_map(|line| {
            let line = line
                .trim()
                .strip_prefix("export ")
                .unwrap_or(line.trim())
                .trim();
            let (key, value) = line.split_once('=')?;
            if key.trim() != "GOOGLE_CLOUD_PROJECT" {
                return None;
            }
            let value = value.trim();
            let value =
                if let Some(quote) = value.chars().next().filter(|c| *c == '\'' || *c == '"') {
                    value[1..].split_once(quote)?.0
                } else {
                    value
                        .split_once(" #")
                        .map(|(value, _)| value)
                        .unwrap_or(value)
                        .trim()
                };
            text(Some(&json!(value)), 512).map(str::to_owned)
        })
        .last()
}

#[cfg(test)]
#[path = "../test/gemini_source.test.rs"]
mod tests;
