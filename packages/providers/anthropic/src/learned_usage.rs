use super::cli;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

const TTL_MS: i64 = 3_600_000;
const MAX_BYTES: u64 = 64 * 1024;
const KINDS: [(&str, &str, u64, Option<&str>, &str); 5] = [
    ("five_hour", "Five hours", 18000, None, "5h"),
    ("seven_day", "Weekly", 604800, None, "7d"),
    (
        "seven_day_opus",
        "Weekly · Opus",
        604800,
        Some("*opus*"),
        "",
    ),
    (
        "seven_day_sonnet",
        "Weekly · Sonnet",
        604800,
        Some("*sonnet*"),
        "",
    ),
    (
        "seven_day_overage_included",
        "Weekly · Fable",
        604800,
        Some("*fable*"),
        "7d_oi",
    ),
];

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    cli::text(value.get(key), 512)
}

fn identity(value: &Value) -> Option<String> {
    if nora_provider::auth::is_api(value) {
        return None;
    }
    let identity = value
        .get("accountIdentity")
        .or_else(|| value.pointer("/extras/accountIdentity"))?;
    let issuer = field(identity, "issuer")?;
    if issuer != "https://claude.ai" {
        return None;
    }
    let subject = field(identity, "subject")?;
    let scope = match identity.get("scope") {
        None | Some(Value::Null) => "",
        Some(_) => field(identity, "scope")?,
    };
    Some(cli::hash(&json!([issuer, subject, scope]).to_string()))
}

#[derive(Clone)]
pub(crate) struct Cache {
    directory: PathBuf,
    key: String,
}
impl Cache {
    pub(crate) fn for_credentials(root: &Path, value: &Value) -> Option<Self> {
        if !root.is_absolute() {
            return None;
        }
        Some(Self {
            directory: root.join("learned-usage"),
            key: identity(value)?,
        })
    }
    fn state_path(&self) -> PathBuf {
        self.directory.join(format!("{}.json", self.key))
    }
    fn read_at(&self, at: i64) -> Option<BTreeMap<String, Value>> {
        let mut bytes = Vec::new();
        let file = File::open(self.state_path()).ok()?;
        if !file.metadata().ok()?.is_file() {
            return None;
        }
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
        if bytes.len() as u64 > MAX_BYTES {
            return None;
        }
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        if value["version"] != 1 || value["account"] != self.key {
            return None;
        }
        let windows = value["windows"].as_object()?;
        let mut output = BTreeMap::new();
        for (kind, _, _, _, _) in KINDS {
            let Some(window) = windows.get(kind) else {
                continue;
            };
            let Some(observed) = window["at"].as_i64().filter(|observed| {
                *observed <= at.saturating_add(60_000) && at.saturating_sub(*observed) < TTL_MS
            }) else {
                continue;
            };
            let Some(used) = window["used"]
                .as_f64()
                .filter(|used| used.is_finite() && (0.0..=1.0).contains(used))
            else {
                continue;
            };
            let reset = window["reset"].as_i64().unwrap_or_default();
            if reset > 0 && reset <= at / 1000 {
                continue;
            }
            output.insert(
                kind.into(),
                json!({"used":used,"reset":reset,"at":observed}),
            );
        }
        Some(output)
    }
    fn write_at(&self, event: &Value, at: i64) -> std::io::Result<()> {
        let updates = parse(event, at);
        if updates.is_empty() {
            return Ok(());
        }
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&self.directory)?;
        if !std::fs::symlink_metadata(&self.directory)?.is_dir() {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(self.directory.join(format!("{}.lock", self.key)))?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match fs2::FileExt::try_lock_exclusive(&lock) {
                Ok(()) => break,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10))
                }
                Err(error) => return Err(error),
            }
        }
        let mut windows = self.read_at(at).unwrap_or_default();
        for (kind, window) in updates {
            if windows
                .get(&kind)
                .and_then(|old| old["at"].as_i64())
                .is_some_and(|old| old > at)
            {
                continue;
            }
            windows.insert(kind, window);
        }
        let bytes = serde_json::to_vec(&json!({"version":1,"account":self.key,"windows":windows}))?;
        let path = self.state_path();
        let staged = self
            .directory
            .join(format!(".{}.{}.tmp", self.key, cli::random()));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&staged)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(&staged, &path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(staged);
        }
        result
    }
    pub(crate) async fn note(&self, event: Value) {
        let cache = self.clone();
        let _ = tokio::task::spawn_blocking(move || cache.write_at(&event, now())).await;
    }
    pub(crate) async fn usage(&self) -> Option<Value> {
        let cache = self.clone();
        tokio::task::spawn_blocking(move || cache.usage_at(now()))
            .await
            .ok()
            .flatten()
    }
    fn usage_at(&self, at: i64) -> Option<Value> {
        let windows = self.read_at(at)?;
        let mut items = Vec::new();
        let mut read_at = 0;
        for (kind, label, span, model, _) in KINDS {
            let Some(window) = windows.get(kind) else {
                continue;
            };
            let observed = window["at"].as_i64()?;
            read_at = read_at.max(observed);
            let observed = chrono::DateTime::from_timestamp_millis(observed)?.to_rfc3339();
            let mut item = json!({"id":kind,"label":label,"usedPercent":window["used"].as_f64()?*100.0,"windowSeconds":span,"countsForRouting":true,"observedAt":observed});
            if let Some(model) = model {
                item["modelIDs"] = json!([model]);
            }
            if let Some(reset) = window["reset"]
                .as_i64()
                .filter(|reset| *reset > 0)
                .and_then(|reset| chrono::DateTime::from_timestamp(reset, 0))
            {
                item["resetsAt"] = json!(reset.to_rfc3339());
            }
            items.push(item);
        }
        if items.is_empty() {
            return None;
        }
        let mut result = nora_provider::usage::parsing::result(&json!({}), items, Vec::new());
        result["readAt"] = json!(read_at);
        result["asOf"] = json!(read_at);
        result["observedAt"] =
            json!(chrono::DateTime::from_timestamp_millis(read_at)?.to_rfc3339());
        Some(result)
    }
    pub(crate) async fn headers(&self) -> BTreeMap<String, String> {
        let cache = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut headers = BTreeMap::new();
            let windows = cache.read_at(now()).unwrap_or_default();
            for (kind, _, _, _, name) in KINDS {
                if name.is_empty() {
                    continue;
                }
                let Some(window) = windows.get(kind) else {
                    continue;
                };
                let Some(reset) = window["reset"].as_i64().filter(|reset| *reset > 0) else {
                    continue;
                };
                headers.insert(
                    format!("anthropic-ratelimit-unified-{name}-utilization"),
                    window["used"].as_f64().unwrap().to_string(),
                );
                headers.insert(
                    format!("anthropic-ratelimit-unified-{name}-reset"),
                    reset.to_string(),
                );
            }
            headers
        })
        .await
        .unwrap_or_default()
    }
}

fn parse(event: &Value, at: i64) -> BTreeMap<String, Value> {
    if event["type"] != "rate_limit_event" {
        return BTreeMap::new();
    }
    let info = &event["rate_limit_info"];
    let mut output = BTreeMap::new();
    for (kind, _, _, _, _) in KINDS {
        let unified = info.pointer(&format!("/unifiedWindows/{kind}"));
        let direct = info["rateLimitType"] == kind;
        let window = match unified {
            Some(window) => window,
            None if direct => info,
            _ => continue,
        };
        let used = window["utilization"]
            .as_f64()
            .or_else(|| (unified.is_none() && info["status"] == "rejected").then_some(1.0));
        let Some(used) = used.filter(|used| used.is_finite() && (0.0..=1.0).contains(used)) else {
            continue;
        };
        let reset = window["resetsAt"].as_i64().unwrap_or_default();
        if reset > 0 && reset <= at / 1000 {
            continue;
        }
        output.insert(kind.into(), json!({"used":used,"reset":reset,"at":at}));
    }
    output
}

#[cfg(test)]
#[path = "../test/learned_usage.test.rs"]
mod tests;
