use super::network::{self, Client};
use base64::Engine;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub(crate) fn hash(value: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(value.as_ref()))
}
pub(crate) fn random() -> String {
    nora_ai::oauth_pkce::random_hex(24)
}
pub(crate) fn expiry(token: &str) -> u64 {
    token
        .split('.')
        .nth(1)
        .and_then(|p| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(p.trim_end_matches('='))
                .ok()
        })
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|v| v["exp"].as_u64())
        .unwrap_or(0)
        .saturating_mul(1000)
}
pub(crate) fn expired() -> PluginError {
    PluginError::authentication("Cursor sign-in expired or changed. Reconnect this account.")
}
pub(crate) fn text(value: Option<&Value>, max: usize) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control))
}
pub(crate) fn token(value: Option<&Value>) -> PluginResult<String> {
    text(value, 64 * 1024)
        .filter(|s| !s.chars().any(char::is_whitespace))
        .map(str::to_owned)
        .ok_or_else(expired)
}
pub(crate) fn read_file(path: &str) -> PluginResult<String> {
    nora_provider_plugin_sdk::require_subscription_accounts()?;
    if !Path::new(path).is_absolute() || path.len() > 16 * 1024 {
        return Err(PluginError::invalid(
            "Select an absolute cursor-agent auth.json path.",
        ));
    }
    let file = std::fs::File::open(path).map_err(|_| expired())?;
    if !file.metadata().map_err(|_| expired())?.is_file() {
        return Err(expired());
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(128 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| expired())?;
    if bytes.len() > 128 * 1024 {
        return Err(expired());
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| expired())?;
    token(value.get("accessToken"))
}
pub(crate) async fn source(value: &Value) -> PluginResult<String> {
    nora_provider_plugin_sdk::require_subscription_accounts()?;
    match value["kind"].as_str() {
        Some("file") => {
            let path = text(value.get("path"), 16 * 1024)
                .ok_or_else(expired)?
                .to_owned();
            tokio::task::spawn_blocking(move || read_file(&path))
                .await
                .map_err(|_| expired())?
        }
        Some("keychain") => {
            #[cfg(target_os = "macos")]
            {
                let mut command = tokio::process::Command::new("/usr/bin/security");
                command
                    .args([
                        "find-generic-password",
                        "-s",
                        "cursor-access-token",
                        "-a",
                        "cursor-user",
                        "-w",
                    ])
                    .kill_on_drop(true);
                let output = tokio::time::timeout(Duration::from_secs(30), command.output())
                    .await
                    .map_err(|_| expired())?
                    .map_err(|_| expired())?;
                if !output.status.success() || output.stdout.len() > 64 * 1024 {
                    return Err(expired());
                }
                let value = String::from_utf8(output.stdout).map_err(|_| expired())?;
                token(Some(&json!(value.trim())))
            }
            #[cfg(not(target_os = "macos"))]
            {
                Err(PluginError::unsupported(
                    "the macOS Keychain source on this platform",
                ))
            }
        }
        _ => Err(expired()),
    }
}
pub(crate) async fn exchange(
    client: &dyn nora_ai::HttpClient,
    key: &str,
    version: &str,
) -> PluginResult<String> {
    let mut request = network::post(
        format!("{}/auth/exchange_user_api_key", network::API),
        b"{}".to_vec(),
        key,
        version,
        "application/json",
    );
    request.headers.remove("connect-protocol-version");
    let value = network::read(network::send(client, request, 20).await?).await?;
    token(value.get("accessToken"))
}
pub(crate) struct Pending {
    pub verifier: String,
    pub uuid: String,
    pub created: u64,
    pub client: Client,
    pub version: String,
}
pub(crate) fn start(client: Client, version: String) -> (Pending, String) {
    let verifier = nora_ai::oauth_pkce::random_hex(32);
    let uuid = super::request::uuid();
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let mut url = url::Url::parse("https://cursor.com/loginDeepControl").unwrap();
    url.query_pairs_mut()
        .append_pair("challenge", &challenge)
        .append_pair("uuid", &uuid)
        .append_pair("mode", "login")
        .append_pair("redirectTarget", "cli");
    (
        Pending {
            verifier,
            uuid,
            created: now(),
            client,
            version,
        },
        url.to_string(),
    )
}
impl Pending {
    pub async fn complete(self) -> PluginResult<Value> {
        let deadline = tokio::time::Instant::now()
            + Duration::from_millis(600_000u64.saturating_sub(now().saturating_sub(self.created)));
        let mut errors = 0;
        let mut pause = 1000;
        for _ in 0..150 {
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            let mut url = url::Url::parse(&format!("{}/auth/poll", network::API)).unwrap();
            url.query_pairs_mut()
                .append_pair("uuid", &self.uuid)
                .append_pair("verifier", &self.verifier);
            let request = network::headers(
                nora_ai::HttpRequest::new(nora_ai::HttpMethod::Get, url.to_string()),
                "",
                &self.version,
            )
            .with_header("content-type", "application/json");
            let response =
                tokio::time::timeout_at(deadline, network::send(self.client.as_ref(), request, 15))
                    .await
                    .map_err(|_| expired())?;
            match response {
                Ok(response) if response.status == 404 => {
                    errors = 0;
                }
                Ok(response) if (200..300).contains(&response.status) => {
                    let value = network::read(response).await?;
                    let access = token(value.get("accessToken"))?;
                    if value.get("refreshToken").is_none() {
                        return Err(expired());
                    }
                    let (email, plan) =
                        network::identify(self.client.as_ref(), &access, &self.version).await?;
                    return Ok(
                        json!({"type":"oauth","authMethod":"browser","access":access,"refresh":value["refreshToken"],"expires":expiry(&access),"accountId":email,"email":email,"label":email,"plan":plan}),
                    );
                }
                Ok(response) if response.status == 403 => return Err(expired()),
                _ => {
                    errors += 1;
                    if errors >= 3 {
                        return Err(PluginError::transport(
                            "Cursor sign-in could not reach the service.",
                        ));
                    }
                }
            }
            tokio::time::sleep_until(
                (tokio::time::Instant::now() + Duration::from_millis(pause)).min(deadline),
            )
            .await;
            pause = (pause * 12 / 10).min(10_000);
        }
        Err(PluginError::authentication("Cursor sign-in timed out."))
    }
}

#[cfg(test)]
#[path = "../test/auth.test.rs"]
mod tests;
