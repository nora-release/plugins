use nora_provider_plugin_sdk::{PluginError, PluginResult};
use nora_provider_subscription_core as core;
use rand::RngCore;
use serde_json::{json, Value};
use std::{io::Read, path::Path, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
    task::JoinHandle,
};
pub(crate) const ORIGIN: &str = "https://xiaohuanxiong.com";
pub(crate) const BASE: &str = "https://xiaohuanxiong.com/api/web/llm/v2";
pub(crate) fn strip(value: &str) -> &str {
    {
        let value = value.trim();
        if value
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("bearer "))
        {
            value[7..].trim()
        } else {
            value
        }
    }
}
pub(crate) fn state() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}
pub(crate) fn uid(claims: &Value) -> String {
    for key in ["sid", "iss"] {
        if let Some(raw) = core::text(&claims[key]) {
            let raw = raw
                .strip_prefix("0x")
                .or_else(|| raw.strip_prefix("0X"))
                .unwrap_or(raw);
            if !raw.is_empty() && raw.len() <= 256 && raw.bytes().all(|b| b.is_ascii_hexdigit()) {
                if let Some(value) = num_bigint::BigUint::parse_bytes(raw.as_bytes(), 16) {
                    return value.to_str_radix(10);
                }
            }
        }
    }
    ["sub", "id", "user_id"]
        .iter()
        .find_map(|key| core::text(&claims[key]))
        .unwrap_or("")
        .to_owned()
}
pub(crate) fn credential(access: &str, refresh: &str, method: &str) -> PluginResult<Value> {
    let access = strip(access);
    if access.is_empty()
        || access.len() > 64 * 1024
        || access.contains(['\r', '\n', '\0'])
        || refresh.contains(['\r', '\n', '\0'])
    {
        return Err(PluginError::authentication(
            "The Raccoon token is missing or invalid.",
        ));
    }
    let claims = core::decode_jwt(access);
    let uid = uid(&claims);
    let name = core::text(&claims["name"]).unwrap_or("");
    let expires = claims["exp"]
        .as_u64()
        .map(|n| n.saturating_mul(1000))
        .unwrap_or(0);
    if expires > 0 && expires <= core::now() {
        return Err(PluginError::authentication(
            "The selected Raccoon token expired. Sign in again.",
        ));
    }
    Ok(
        json!({"type":"oauth","authMethod":method,"provider":"raccoon","access":access,"refresh":strip(refresh),"expires":expires,"uid":uid,"name":name,"accountId":uid,"label":if name.is_empty(){"Raccoon account"}else{name}}),
    )
}
pub(crate) fn read_source(path: &str) -> PluginResult<Value> {
    nora_provider_plugin_sdk::require_subscription_accounts()?;
    let path = if let Some(rest) = path.strip_prefix("~/") {
        std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .ok_or_else(|| PluginError::invalid("Select an absolute Raccoon account file."))?
            .join(rest)
    } else {
        Path::new(path).to_owned()
    };
    if !path.is_absolute() {
        return Err(PluginError::invalid(
            "Select an absolute Raccoon account file.",
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
        PluginError::authentication("The selected Raccoon account file could not be read.")
    })?;
    let metadata = file.metadata().map_err(|_| {
        PluginError::authentication("The selected Raccoon account file could not be read.")
    })?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 256 * 1024 {
        return Err(PluginError::invalid(
            "Select a regular Raccoon account JSON file smaller than 256 KiB.",
        ));
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(256 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| PluginError::authentication("The Raccoon account file could not be read."))?;
    if bytes.len() > 256 * 1024 {
        return Err(PluginError::invalid(
            "The Raccoon account file grew beyond its size limit.",
        ));
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| PluginError::invalid("The selected Raccoon account file is invalid JSON."))?;
    let token = core::required(&value, "access_token")?;
    let mut result = credential(token, "", "desktop")?;
    if core::text(&result["uid"]).is_none() {
        return Err(PluginError::authentication(
            "The Raccoon desktop token did not identify its account.",
        ));
    }
    if result["expires"]
        .as_u64()
        .is_some_and(|n| n > 0 && n <= core::now() + 60_000)
    {
        return Err(PluginError::authentication(
            "Refresh the selected account in the Raccoon desktop app first.",
        ));
    }
    result["source"] = json!({"kind":"desktop","path":path});
    Ok(result)
}
pub(crate) fn manual(raw: &str, expected: &str) -> PluginResult<Value> {
    let raw = raw.trim();
    if raw.len() > 128 * 1024 {
        return Err(PluginError::invalid("The Raccoon callback is too long."));
    }
    if raw.starts_with('{') {
        let value: Value = serde_json::from_str(raw)
            .map_err(|_| PluginError::invalid("The pasted Raccoon token JSON is invalid."))?;
        return credential(
            core::text(&value["access_token"])
                .or_else(|| core::text(&value["accessToken"]))
                .unwrap_or(""),
            core::text(&value["refresh_token"])
                .or_else(|| core::text(&value["refreshToken"]))
                .unwrap_or(""),
            "manual",
        )
        .map(|value| json!({"direct":value}));
    }
    if !raw.contains("://") && raw.split('.').count() == 3 {
        return credential(raw, "", "manual").map(|value| json!({"direct":value}));
    }
    let url = url::Url::parse(raw).map_err(|_| {
        PluginError::invalid("Paste the Raccoon authorization callback URL or a JWT token.")
    })?;
    if !["office-raccoon", "http", "https"].contains(&url.scheme()) {
        return Err(PluginError::invalid("The Raccoon callback URL is invalid."));
    }
    let values = url.query_pairs().collect::<Vec<_>>();
    let states: Vec<_> = values.iter().filter(|(key, _)| key == "state").collect();
    if states.len() > 1
        || states
            .first()
            .is_some_and(|(_, state)| !state.is_empty() && state != expected)
    {
        return Err(PluginError::authentication(
            "This callback belongs to another Raccoon sign-in.",
        ));
    }
    let codes: Vec<_> = values.iter().filter(|(key, _)| key == "code").collect();
    if codes.len() != 1 || codes[0].1.is_empty() || codes[0].1.len() > 4096 {
        return Err(PluginError::invalid(
            "The Raccoon callback has no valid authorization code.",
        ));
    }
    Ok(json!({"code":codes[0].1}))
}
pub(crate) fn authorize_url(state: &str, redirect: Option<&str>) -> PluginResult<String> {
    let mut url = url::Url::parse(&format!("{ORIGIN}/code/authorize")).unwrap();
    url.query_pairs_mut()
        .append_pair("login_source", "desktop")
        .append_pair("appname", "办公小浣熊客户端")
        .append_pair("state", state);
    if let Some(redirect) = redirect {
        url.query_pairs_mut()
            .append_pair("desktop_callback", "loopback")
            .append_pair("redirect", redirect);
    }
    Ok(url.into())
}
pub(crate) struct Browser {
    pub until: u64,
    pub receive: oneshot::Receiver<PluginResult<String>>,
    task: JoinHandle<()>,
}
impl Drop for Browser {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub(crate) async fn browser(state: String) -> PluginResult<(String, Browser)> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|_| PluginError::transport("The Raccoon browser callback could not start."))?;
    let address = listener
        .local_addr()
        .map_err(|_| PluginError::transport("The Raccoon browser callback could not start."))?;
    let origin = address.to_string();
    let until = core::now() + 600_000;
    let url = authorize_url(&state, Some(&format!("http://{origin}/auth/callback")))?;
    let (send, receive) = oneshot::channel();
    let task = tokio::spawn(async move {
        let mut sender = Some(send);
        for _ in 0..1000 {
            let remaining = until.saturating_sub(core::now());
            if remaining == 0 {
                break;
            }
            let Ok(Ok((mut socket, _))) =
                tokio::time::timeout(Duration::from_millis(remaining), listener.accept()).await
            else {
                break;
            };
            let mut data = Vec::new();
            let mut buffer = [0u8; 1024];
            let mut valid = false;
            while data.len() < 16 * 1024 {
                match tokio::time::timeout(Duration::from_secs(5), socket.read(&mut buffer)).await {
                    Ok(Ok(0)) | Err(_) | Ok(Err(_)) => break,
                    Ok(Ok(n)) => {
                        data.extend_from_slice(&buffer[..n]);
                        if data.windows(4).any(|s| s == b"\r\n\r\n") {
                            valid = true;
                            break;
                        }
                    }
                }
            }
            let request = String::from_utf8_lossy(&data);
            let parsed = if valid {
                callback_code(&request, &origin, &state)
            } else {
                Err(PluginError::invalid(
                    "The browser callback request is incomplete.",
                ))
            };
            let status = if parsed.is_ok() { 200 } else { 400 };
            let body = if parsed.is_ok() {
                "Approval received. Return to Nora to finish signing in."
            } else {
                "This callback does not belong to the current sign-in."
            };
            let reply=format!("HTTP/1.1 {status} {}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",if status==200{"OK"}else{"Bad Request"},body.len());
            let _ = socket.write_all(reply.as_bytes()).await;
            let _ = socket.shutdown().await;
            match parsed {
                Ok(code) => {
                    if let Some(sender) = sender.take() {
                        let _ = sender.send(Ok(code));
                    }
                    return;
                }
                Err(error) if error.code == "authorization_denied" => {
                    if let Some(sender) = sender.take() {
                        let _ = sender.send(Err(error));
                    }
                    return;
                }
                _ => {}
            }
        }
        if let Some(sender) = sender.take() {
            let _ = sender.send(Err(PluginError::authentication(
                "The Raccoon browser sign-in expired.",
            )));
        }
    });
    Ok((
        url,
        Browser {
            until,
            receive,
            task,
        },
    ))
}
pub(crate) fn callback_code(request: &str, origin: &str, state: &str) -> PluginResult<String> {
    let mut lines = request.split("\r\n");
    let mut first = lines.next().unwrap_or("").split_whitespace();
    if first.next() != Some("GET") {
        return Err(PluginError::invalid("Only GET callbacks are accepted."));
    }
    let path = first
        .next()
        .ok_or_else(|| PluginError::invalid("The callback path is invalid."))?;
    let host: Vec<_> = lines
        .filter_map(|line| line.split_once(':'))
        .filter(|(key, _)| key.eq_ignore_ascii_case("host"))
        .collect();
    if host.len() != 1 || host[0].1.trim() != origin {
        return Err(PluginError::invalid("The callback host is invalid."));
    }
    let url = url::Url::parse(&format!("http://{origin}{path}"))
        .map_err(|_| PluginError::invalid("The callback path is invalid."))?;
    if !path.starts_with('/') || url.path() != "/auth/callback" {
        return Err(PluginError::invalid("The callback path is invalid."));
    }
    let values = url.query_pairs().collect::<Vec<_>>();
    let states: Vec<_> = values.iter().filter(|(key, _)| key == "state").collect();
    if states.len() != 1 || states[0].1 != state {
        return Err(PluginError::authentication(
            "The callback state does not match.",
        ));
    }
    if values.iter().any(|(key, _)| key == "error") {
        return Err(PluginError::new(
            "authorization_denied",
            "Raccoon sign-in was refused.",
        ));
    }
    let codes: Vec<_> = values.iter().filter(|(key, _)| key == "code").collect();
    if codes.len() != 1 || codes[0].1.is_empty() || codes[0].1.len() > 4096 {
        return Err(PluginError::invalid("The callback code is invalid."));
    }
    Ok(codes[0].1.to_string())
}
#[cfg(test)]
#[path = "../test/auth.test.rs"]
mod tests;
