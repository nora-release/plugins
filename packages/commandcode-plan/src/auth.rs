use nora_provider_plugin_sdk::{PluginError, PluginResult};
use nora_provider_subscription_core as core;
use rand::RngCore;
use serde_json::{json, Value};
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
    task::JoinHandle,
};
const ORIGINS: [&str; 2] = ["https://commandcode.ai", "https://staging.commandcode.ai"];
pub(crate) fn state() -> String {
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}
pub(crate) fn credential(input: &Value, method: &str) -> PluginResult<Value> {
    let key = core::required(input, "apiKey")?;
    Ok(
        json!({"type":"api","provider":"commandcode-plan","authMethod":method,"key":key,"expires":0,"userId":input["userId"],"userName":input["userName"],"keyName":input["keyName"]}),
    )
}
pub(crate) fn read_source(path: &str) -> PluginResult<(PathBuf, Value)> {
    nora_provider_plugin_sdk::require_subscription_accounts()?;
    read_source_with_home(path, std::env::var_os("HOME").map(PathBuf::from))
}

fn read_source_with_home(path: &str, home: Option<PathBuf>) -> PluginResult<(PathBuf, Value)> {
    let path = path.trim();
    let path = if path.is_empty() {
        "~/.commandcode/auth.json"
    } else {
        path
    };
    let path = if let Some(rest) = path.strip_prefix("~/") {
        home.ok_or_else(|| PluginError::invalid("Select an absolute Command Code account file."))?
            .join(rest)
    } else {
        Path::new(path).to_owned()
    };
    if !path.is_absolute() {
        return Err(PluginError::invalid(
            "Select an absolute Command Code account file.",
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(&path).map_err(|_| {
        PluginError::authentication("The selected Command Code account file could not be read.")
    })?;
    let metadata = file
        .metadata()
        .map_err(|_| PluginError::authentication("The selected account file could not be read."))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 256 * 1024 {
        return Err(PluginError::invalid(
            "Select a regular account JSON file smaller than 256 KiB.",
        ));
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(256 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| PluginError::authentication("The selected account file could not be read."))?;
    if bytes.len() > 256 * 1024 {
        return Err(PluginError::invalid(
            "The selected account file grew beyond its size limit.",
        ));
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
        PluginError::invalid("The selected Command Code account file is invalid JSON.")
    })?;
    let value = credential(&value, "cli")?;
    Ok((path, value))
}
pub(crate) struct Browser {
    pub until: u64,
    pub receive: oneshot::Receiver<PluginResult<Value>>,
    accepted: Arc<AtomicBool>,
    task: JoinHandle<()>,
}
impl Drop for Browser {
    fn drop(&mut self) {
        if !self.accepted.load(Ordering::SeqCst) {
            self.task.abort();
        }
    }
}
struct Incoming {
    method: String,
    path: String,
    headers: std::collections::BTreeMap<String, String>,
    body: Vec<u8>,
}
fn parse(bytes: &[u8], host: &str) -> PluginResult<Incoming> {
    let split = bytes
        .windows(4)
        .position(|s| s == b"\r\n\r\n")
        .ok_or_else(|| PluginError::invalid("Incomplete callback."))?;
    if split > 16 * 1024 {
        return Err(PluginError::invalid("Callback headers are too large."));
    }
    let head = std::str::from_utf8(&bytes[..split])
        .map_err(|_| PluginError::invalid("Invalid callback headers."))?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split_whitespace();
    let method = first.next().unwrap_or("");
    let path = first.next().unwrap_or("");
    if first.next() != Some("HTTP/1.1")
        || first.next().is_some()
        || !path.starts_with('/')
        || path.starts_with("//")
    {
        return Err(PluginError::invalid("Invalid callback request."));
    }
    let mut headers = std::collections::BTreeMap::new();
    for line in lines {
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| PluginError::invalid("Invalid callback header."))?;
        let key = key.trim().to_ascii_lowercase();
        if headers.insert(key, value.trim().to_owned()).is_some() {
            return Err(PluginError::invalid("Repeated callback header."));
        }
    }
    if headers.get("host").map(String::as_str) != Some(host)
        || headers.contains_key("transfer-encoding")
    {
        return Err(PluginError::invalid("Invalid callback host or encoding."));
    }
    if headers
        .get("origin")
        .is_some_and(|origin| !ORIGINS.contains(&origin.as_str()))
    {
        return Err(PluginError::invalid(
            "This callback has an unexpected origin.",
        ));
    }
    let length = headers
        .get("content-length")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    if length > 10_000 || bytes.len() - split - 4 != length {
        return Err(PluginError::invalid("Invalid callback body length."));
    }
    Ok(Incoming {
        method: method.to_owned(),
        path: path.split('?').next().unwrap_or("").to_owned(),
        headers,
        body: bytes[split + 4..].to_vec(),
    })
}
fn approve(incoming: &Incoming, state: &str) -> PluginResult<Value> {
    if incoming.path != "/callback" || incoming.method != "POST" {
        return Err(PluginError::invalid(
            "Invalid Command Code callback path or method.",
        ));
    }
    let value: Value = if incoming
        .headers
        .get("content-type")
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("application/json"))
    {
        serde_json::from_slice(&incoming.body)
            .map_err(|_| PluginError::invalid("Invalid callback JSON."))?
    } else {
        let mut map = serde_json::Map::new();
        for (key, value) in url::form_urlencoded::parse(&incoming.body) {
            if map.insert(key.into_owned(), json!(value)).is_some() {
                return Err(PluginError::invalid("Repeated callback field."));
            }
        }
        Value::Object(map)
    };
    if value["state"] != state {
        return Err(PluginError::authentication(
            "This callback belongs to another Command Code sign-in.",
        ));
    }
    if value.get("error").is_some() {
        return Err(PluginError::new(
            "authorization_denied",
            "The Command Code sign-in was denied.",
        ));
    }
    credential(&value, "oauth")
}
pub(crate) async fn browser() -> PluginResult<(String, Browser)> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|_| PluginError::transport("The Command Code callback could not start."))?;
    let address = listener
        .local_addr()
        .map_err(|_| PluginError::transport("The callback address is unavailable."))?;
    let host = format!("127.0.0.1:{}", address.port());
    let state = state();
    let mut url = url::Url::parse("https://commandcode.ai/studio/auth/cli").unwrap();
    url.query_pairs_mut()
        .append_pair("callback", &format!("http://{host}/callback"))
        .append_pair("state", &state)
        .append_pair("mode", "redirect");
    let until = core::now() + 600_000;
    let (send, receive) = oneshot::channel();
    let accepted = Arc::new(AtomicBool::new(false));
    let done = accepted.clone();
    let task = tokio::spawn(async move {
        let mut sender = Some(send);
        let mut deadline = tokio::time::Instant::now() + Duration::from_secs(600);
        for _ in 0..1000 {
            let Ok(Ok((mut socket, _))) =
                tokio::time::timeout_at(deadline, listener.accept()).await
            else {
                break;
            };
            let read = async {
                let mut raw = Vec::new();
                let mut chunk = [0u8; 2048];
                loop {
                    let count = socket.read(&mut chunk).await?;
                    if count == 0 {
                        break;
                    }
                    raw.extend_from_slice(&chunk[..count]);
                    if raw.len() > 26_384 {
                        break;
                    }
                    if let Some(split) = raw.windows(4).position(|s| s == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&raw[..split]);
                        let length = header
                            .lines()
                            .filter_map(|line| line.split_once(':'))
                            .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                            .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if length > 10_000 || raw.len() >= split + 4 + length {
                            break;
                        }
                    }
                }
                Ok::<_, std::io::Error>(raw)
            };
            let incoming = match tokio::time::timeout(Duration::from_secs(5), read).await {
                Ok(Ok(raw)) => parse(&raw, &host),
                _ => Err(PluginError::invalid("Incomplete callback.")),
            };
            let (mut status, mut body, mut extra) =
                (400, "Invalid callback.".to_owned(), String::new());
            if let Ok(incoming) = incoming {
                let origin = incoming
                    .headers
                    .get("origin")
                    .map(String::as_str)
                    .unwrap_or(ORIGINS[0]);
                extra=format!("Access-Control-Allow-Origin: {origin}\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\nVary: Origin\r\n");
                if incoming
                    .headers
                    .get("access-control-request-private-network")
                    .is_some_and(|v| v == "true")
                {
                    extra.push_str("Access-Control-Allow-Private-Network: true\r\n");
                }
                if incoming.method == "OPTIONS" && incoming.path == "/callback" {
                    status = 204;
                    body.clear();
                } else if incoming.method == "GET"
                    && incoming.path == "/callback/complete"
                    && done.load(Ordering::SeqCst)
                {
                    status = 200;
                    body = "Approval received. Return to Nora to finish signing in.".into();
                } else if sender.is_some() {
                    match approve(&incoming, &state) {
                        Ok(value) => {
                            if let Some(sender) = sender.take() {
                                let _ = sender.send(Ok(value));
                            }
                            done.store(true, Ordering::SeqCst);
                            deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                            if incoming
                                .headers
                                .get("content-type")
                                .is_some_and(|v| v.starts_with("application/json"))
                            {
                                status = 200;
                                body = "{\"success\":true}".into();
                                extra.push_str("Content-Type: application/json\r\n");
                            } else {
                                status = 303;
                                body.clear();
                                extra.push_str("Location: /callback/complete\r\n");
                            }
                        }
                        Err(error) if error.code == "authorization_denied" => {
                            if let Some(sender) = sender.take() {
                                let _ = sender.send(Err(error));
                            }
                            deadline = tokio::time::Instant::now() + Duration::from_secs(1);
                        }
                        Err(_) => {}
                    }
                }
            }
            let reason = match status {
                200 => "OK",
                204 => "No Content",
                303 => "See Other",
                _ => "Bad Request",
            };
            let response=format!("HTTP/1.1 {status} {reason}\r\n{extra}Cache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",body.len());
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
        if let Some(sender) = sender {
            let _ = sender.send(Err(PluginError::authentication(
                "The Command Code browser sign-in expired.",
            )));
        }
    });
    Ok((
        url.into(),
        Browser {
            until,
            receive,
            accepted,
            task,
        },
    ))
}
#[cfg(test)]
#[path = "../test/auth.test.rs"]
mod tests;
