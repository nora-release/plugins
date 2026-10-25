use crate::*;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
const CAMPAIGNS: &str = "/sash/api/v1/me/campaigns";
async fn page(
    provider: &Qoder,
    client: &dyn HttpClient,
    credentials: &mut Value,
    path: &str,
    method: HttpMethod,
    body: Option<Value>,
    machine: &BTreeMap<String, String>,
) -> PluginResult<Value> {
    let deployment = provider.deployment(credentials)?;
    let pat = credentials.get("patAuth").and_then(Value::as_bool) == Some(true)
        && text(credentials, "deviceToken").is_empty();
    for attempt in 0..2 {
        let token = text(credentials, if pat { "access" } else { "deviceToken" });
        if token.is_empty() {
            return Err(transport(
                "Qoder has no account-page token. Sign in again to view usage; chat can continue.",
            ));
        }
        let mut headers = BTreeMap::from([
            ("accept".into(), "application/json".into()),
            ("authorization".into(), format!("Bearer {token}")),
            ("cosy-clienttype".into(), "10".into()),
            ("user-agent".into(), "Qoder".into()),
        ]);
        headers.extend(machine.clone());
        let bytes = body.as_ref().map(|body| body.to_string().into_bytes());
        if bytes.is_some() {
            headers.insert("content-type".into(), "application/json".into());
        }
        let (status, _, bytes) = read(
            send(
                client,
                HttpRequest {
                    method: method.clone(),
                    url: format!("{}{path}", deployment.openapi),
                    headers,
                    body: bytes,
                },
                20,
            )
            .await?,
            4 * 1024 * 1024,
        )
        .await?;
        if [401, 403].contains(&status) && attempt == 0 && !pat {
            if credentials.get("deviceChat").and_then(Value::as_bool) == Some(true) {
                return Err(authentication(
                    "Renew the Qoder CN account sign-in before reading its account page.",
                ));
            }
            let next = auth::renew(provider, client, credentials, true)
                .await
                .map_err(|error| transport(error.message))?;
            nora_provider_plugin_sdk::save_credentials(next.clone()).await?;
            *credentials = next;
            continue;
        }
        if status != 200 {
            return Err(transport(format!(
                "Qoder account page returned HTTP {status}; chat can continue."
            )));
        }
        return serde_json::from_slice(&bytes)
            .map_err(|_| transport("Qoder returned unreadable account-page JSON."));
    }
    Err(transport(
        "Qoder did not accept the renewed account-page token.",
    ))
}
pub(crate) async fn fetch(
    provider: &Qoder,
    client: &dyn HttpClient,
    mut credentials: Value,
) -> PluginResult<Value> {
    let current = auth::fresh(provider, client, &credentials).await?;
    let renewed = current != credentials;
    credentials = current;
    let value = page(
        provider,
        client,
        &mut credentials,
        "/sash/api/v2/me/usage",
        HttpMethod::Get,
        None,
        &BTreeMap::new(),
    )
    .await?;
    let mut result = parse(&value)?;
    if renewed {
        result["signIn"] = json!("renewed");
    }
    Ok(result)
}
pub(crate) fn parse(value: &Value) -> PluginResult<Value> {
    if text(value, "displayMode") == "enterprise" {
        return Ok(json!({"plan":"Enterprise","windows":[],"signIn":"kept"}));
    }
    let raw = value
        .get("qoderUsage")
        .filter(|raw| raw.is_object())
        .ok_or_else(|| transport("Qoder returned no account quota data."))?;
    if text(value, "displayMode") != "qoder" {
        return Err(transport("Qoder returned an unknown usage display mode."));
    }
    let field = |a: &str, b: &str| raw.get(a).or_else(|| raw.get(b));
    let mut windows = Vec::new();
    fn add(windows: &mut Vec<Value>, value: Option<&Value>, name: &str) {
        let Some(value) = value.filter(|value| value.is_object()) else {
            return;
        };
        for key in ["total", "cap", "used", "remaining"] {
            if value
                .get(key)
                .is_some_and(|value| !value.is_null() && !value.is_number())
            {
                return;
            }
        }
        for key in ["name", "unit"] {
            if value
                .get(key)
                .is_some_and(|value| !value.is_null() && !value.is_string())
            {
                return;
            }
        }
        let Some(total) = value
            .get("total")
            .or_else(|| value.get("cap"))
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && *value > 0.0)
        else {
            return;
        };
        let Some(used) = value
            .get("used")
            .and_then(Value::as_f64)
            .or_else(|| {
                value
                    .get("remaining")
                    .and_then(Value::as_f64)
                    .map(|remaining| total - remaining)
            })
            .filter(|used| used.is_finite() && *used >= 0.0)
        else {
            return;
        };
        let label = if text(value, "name").is_empty() {
            name
        } else {
            text(value, "name")
        };
        let unit = if text(value, "unit").is_empty() {
            "credits"
        } else {
            text(value, "unit")
        };
        windows.push(json!({"name":label,"used":(100.0*used/total).min(100.0),"amount":used,"limit":total,"unit":unit,"display":format!("{used} / {total} {unit}")}));
    }
    add(&mut windows, field("userQuota", "user_quota"), "Credits");
    add(
        &mut windows,
        field("addOnQuota", "add_on_quota"),
        "Add-on credits",
    );
    add(
        &mut windows,
        field("orgResourcePackage", "org_resource_package"),
        "Shared credits",
    );
    for package in field("dedicatedResourcePackages", "dedicated_resource_packages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(1000)
    {
        add(&mut windows, Some(package), "Dedicated credits");
    }
    let mut result = json!({"windows":windows,"signIn":"kept"});
    if let Some(plan) = field("userType", "user_type").and_then(Value::as_str) {
        result["plan"] = json!(plan);
    }
    if let Some(until) = field("expiresAt", "expires_at").and_then(date) {
        result["until"] = json!(stamp(until));
    }
    Ok(result)
}
pub(crate) fn date(value: &Value) -> Option<u64> {
    if let Some(text) = value.as_str() {
        if let Ok(date) = chrono::DateTime::parse_from_rfc3339(text) {
            return (date.timestamp_millis() > 0).then_some(date.timestamp_millis() as u64);
        }
    }
    let value = number(value).filter(|value| *value > 0.0)?;
    let value = if value < 1e12 { value * 1000.0 } else { value };
    if value > i64::MAX as f64 {
        return None;
    }
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(value as i64)?;
    Some(value as u64)
}
fn stamp(at: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(at.min(i64::MAX as u64) as i64)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
pub(crate) async fn checkin(
    provider: &Qoder,
    client: &dyn HttpClient,
    credentials: Value,
) -> PluginResult<Value> {
    checkin_with_status(provider, client, credentials, false).await
}
pub(crate) async fn checkin_status(
    provider: &Qoder,
    client: &dyn HttpClient,
    credentials: Value,
) -> PluginResult<Value> {
    checkin_with_status(provider, client, credentials, true).await
}
async fn checkin_with_status(
    provider: &Qoder,
    client: &dyn HttpClient,
    mut credentials: Value,
    status_only: bool,
) -> PluginResult<Value> {
    let machine = if provider.site == Site::International {
        machine(provider, text(&credentials, "uid")).await?
    } else {
        BTreeMap::new()
    };
    let value = page(
        provider,
        client,
        &mut credentials,
        CAMPAIGNS,
        HttpMethod::Get,
        None,
        &machine,
    )
    .await?;
    let campaigns = value
        .get("campaigns")
        .and_then(Value::as_array)
        .filter(|c| c.len() <= 1000)
        .ok_or_else(|| transport("Qoder returned no valid campaigns list."))?
        .iter()
        .filter(|c| text(c, "actionType") == "CLAIM_BENEFIT")
        .collect::<Vec<_>>();
    let current = now();
    let when = |v: &Value| {
        number(v)
            .filter(|n| *n >= 0.0 && *n <= i64::MAX as f64)
            .map(|n| {
                if n > 1e12 {
                    n as u64
                } else {
                    (n * 1000.0) as u64
                }
            })
    };
    let active = |c: &&Value| {
        when(&c["startAt"]).is_some_and(|at| at <= current)
            && when(&c["endAt"]).is_some_and(|at| at > current)
    };
    let until = campaigns
        .iter()
        .filter(|c| active(c))
        .filter_map(|c| when(&c["endAt"]))
        .min();
    let finish = |mut result: Value| {
        if let Some(until) = until {
            result["validUntil"] = json!(stamp(until));
        }
        result
    };
    let claimable = campaigns
        .iter()
        .copied()
        .filter(|c| text(c, "claimStatus") == "CLAIMABLE")
        .collect::<Vec<_>>();
    if status_only && !claimable.is_empty() {
        return Ok(finish(json!({"outcome":"ready","retrySafe":true})));
    }
    if claimable.is_empty() {
        if let Some(done) = campaigns
            .iter()
            .find(|c| text(c, "claimStatus") == "CLAIMED" && active(c))
        {
            return Ok(finish(outcome(
                "done",
                done.pointer("/benefit/amount").and_then(number),
            )));
        }
        return if campaigns.is_empty() {
            Ok(outcome("inactive", None))
        } else {
            Ok(json!({"outcome":"failed","retrySafe":true}))
        };
    }
    let mut reward = 0.0;
    let mut has_reward = false;
    for campaign in claimable {
        let id = campaign
            .get("campaignId")
            .filter(|v| v.is_string() || v.is_number())
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| v.to_string())
            })
            .filter(|v| !v.trim().is_empty() && v.len() <= 512 && !v.chars().any(char::is_control))
            .ok_or_else(|| transport("Qoder's claimable campaign has no identity."))?;
        let path = format!("{CAMPAIGNS}/{}/claim", crate::site::encode_path(&id));
        let claimed = page(
            provider,
            client,
            &mut credentials,
            &path,
            HttpMethod::Post,
            Some(json!({})),
            &machine,
        )
        .await?;
        let state = claimed
            .get("status")
            .or_else(|| claimed.pointer("/data/status"))
            .and_then(Value::as_str);
        if state != Some("CLAIMED") {
            return Err(transport("Qoder did not confirm the credit claim."));
        }
        if let Some(amount) = claimed
            .pointer("/benefit/amount")
            .or_else(|| claimed.pointer("/data/benefit/amount"))
            .or_else(|| campaign.pointer("/benefit/amount"))
            .and_then(number)
            .filter(|n| *n >= 0.0)
        {
            has_reward = true;
            reward += amount;
        }
    }
    Ok(finish(outcome("claimed", has_reward.then_some(reward))))
}

fn outcome(value: &str, credit: Option<f64>) -> Value {
    let mut result = json!({"outcome":value});
    if let Some(credit) = credit.filter(|credit| *credit > 0.0) {
        result["credit"] = json!(credit);
    }
    result
}
async fn machine(provider: &Qoder, uid: &str) -> PluginResult<BTreeMap<String, String>> {
    if uid.is_empty() || uid.len() > 4096 {
        return Err(authentication(
            "Qoder did not identify the account. Sign in again.",
        ));
    }
    let mut machines = provider.machines.lock().await;
    machines.retain(|_, (_, at)| now().saturating_sub(*at) < 3_600_000);
    if let Some((headers, _)) = machines.get(uid) {
        return Ok(headers.clone());
    }
    let executable = runtime().await?;
    let headers = runtime_identity(&executable, uid).await?;
    if machines.len() >= 128 {
        if let Some(first) = machines
            .iter()
            .min_by_key(|(_, (_, at))| at)
            .map(|(key, _)| key.clone())
        {
            machines.remove(&first);
        }
    }
    machines.insert(uid.into(), (headers.clone(), now()));
    Ok(headers)
}
async fn runtime() -> PluginResult<PathBuf> {
    if let Some(path) = std::env::var_os("QODER_RUNTIME_INFO") {
        let path = PathBuf::from(path);
        return if path.is_absolute()
            && tokio::fs::metadata(&path)
                .await
                .is_ok_and(|metadata| metadata.is_file())
        {
            Ok(path)
        } else {
            Err(transport(
                "The configured Qoder device identity runtime is unavailable.",
            ))
        };
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| transport("The Qoder home directory is unavailable."))?;
    let os = std::env::consts::OS;
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        arch => arch,
    };
    let filename = if os == "windows" {
        "runtime-info.exe"
    } else {
        "runtime-info"
    };
    let mut paths = Vec::new();
    match os {
        "macos" => {
            for root in [PathBuf::from("/Applications"), home.join("Applications")] {
                paths.push(
                    root.join("Qoder.app/Contents/Resources/umid")
                        .join(filename),
                );
            }
        }
        "windows" => {
            let root = std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join("AppData/Local"));
            paths.push(root.join("Programs/Qoder/resources/umid").join(filename));
        }
        "linux" => paths.push(PathBuf::from("/opt/Qoder/resources/umid").join(filename)),
        _ => {}
    }
    for path in paths {
        if tokio::fs::metadata(&path)
            .await
            .is_ok_and(|metadata| metadata.is_file())
        {
            return Ok(path);
        }
    }
    let config = std::env::var_os("QODER_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("QODER_CLI_HOME")
                .or_else(|| std::env::var_os("GEMINI_CLI_HOME"))
                .map(PathBuf::from)
                .unwrap_or(home)
                .join(std::env::var_os("QODER_CONFIG_DIR_NAME").unwrap_or_else(|| ".qoder".into()))
        });
    let mut entries=tokio::fs::read_dir(config.join(".bin")).await.map_err(|_|transport("Qoder check-in needs Qoder's installed device identity runtime. Install Qoder or set QODER_RUNTIME_INFO."))?;
    let platform = match os {
        "macos" => "darwin",
        "windows" => "win32",
        value => value,
    };
    let mut found = Vec::new();
    let mut count = 0;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|_| transport("The Qoder runtime directory could not be read."))?
    {
        count += 1;
        if count > 4096 {
            break;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = entry
            .file_type()
            .await
            .map_err(|_| transport("The Qoder runtime file could not be inspected."))?;
        let path =
            if os == "windows" && kind.is_dir() && name.starts_with(&format!("umid-win32-{arch}-"))
            {
                entry.path().join(filename)
            } else if kind.is_file()
                && name.starts_with(&format!("runtime-info-{platform}-{arch}-"))
                && !name.ends_with(".tmp")
            {
                entry.path()
            } else {
                continue;
            };
        if let Ok(metadata) = tokio::fs::metadata(&path).await {
            if metadata.is_file() {
                found.push((metadata.modified().unwrap_or(std::time::UNIX_EPOCH), path));
            }
        }
    }
    found.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    found.into_iter().next().map(|(_,path)|path).ok_or_else(||transport("Qoder check-in needs Qoder's installed device identity runtime. Install Qoder or set QODER_RUNTIME_INFO."))
}
async fn runtime_identity(path: &Path, uid: &str) -> PluginResult<BTreeMap<String, String>> {
    let mut command = tokio::process::Command::new(path);
    command.arg("3");
    if std::env::consts::OS != "linux" {
        command.arg("--account-stdin");
    }
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|_| transport("Qoder's device identity runtime could not start."))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| transport("Qoder's device identity output is unavailable."))?;
    let bytes = tokio::time::timeout(Duration::from_secs(20), async {
        if let Some(mut stdin) = child.stdin.take() {
            if std::env::consts::OS != "linux" {
                stdin
                    .write_all(format!("{}\n", json!({"account":uid})).as_bytes())
                    .await
                    .map_err(|_| transport("Qoder's device identity input could not be sent."))?;
            }
        }
        let mut bytes = Vec::new();
        stdout
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| transport("Qoder's device identity could not be read."))?;
        if bytes.len() > 1024 * 1024 {
            return Err(transport("Qoder's device identity output exceeds 1 MiB."));
        }
        let status = child
            .wait()
            .await
            .map_err(|_| transport("Qoder's device identity process stopped."))?;
        if !status.success() {
            return Err(transport(
                "Qoder's device identity runtime failed. Restart or update Qoder.",
            ));
        }
        Ok::<_, PluginError>(bytes)
    })
    .await
    .map_err(|_| transport("Qoder's device identity runtime timed out."))??;
    let line = bytes
        .split(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    let value: Value = serde_json::from_slice(line)
        .map_err(|_| transport("Qoder returned an invalid device identity."))?;
    let mut headers = BTreeMap::new();
    for (header, field) in [
        ("cosy-machinetoken", "machineToken"),
        ("cosy-machinetype", "machineType"),
    ] {
        let value = text(&value, field).trim();
        if value.is_empty()
            || value.len() > 4096
            || !value.bytes().all(|byte| (b' '..=b'~').contains(&byte))
        {
            return Err(transport("Qoder returned an invalid device identity."));
        }
        headers.insert(header.into(), value.into());
    }
    Ok(headers)
}
#[cfg(test)]
#[path = "../test/usage.test.rs"]
mod tests;
