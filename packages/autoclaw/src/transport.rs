use super::*;
use md5::{Digest, Md5};

const APP_ID: &str = "100003";
const APP_KEY: &str = "38d2391985e2369a5fb8227d8e6cd5e5";
pub(super) const VERSION: &str = "1.18.1";
pub(super) const CN_VERSION: &str = "2.1.0";
pub(super) fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "mac"
    } else if cfg!(target_os = "windows") {
        "win"
    } else {
        "linux"
    }
}
pub(super) fn agent(version: &str) -> String {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        "x86" => "ia32",
        other => other,
    };
    format!("AutoClaw/{version} ({}-{})", platform(), arch)
}

#[cfg(test)]
#[path = "../test/transport.test.rs"]
mod tests;
pub(super) fn headers(access: &str, at: u64) -> PluginResult<BTreeMap<String, String>> {
    let stamp = (at / 1000).to_string();
    let sign = format!("{:x}", Md5::digest(format!("{APP_ID}&{stamp}&{APP_KEY}")));
    let mut headers = BTreeMap::from([
        ("content-type".into(), "application/json".into()),
        ("accept".into(), "*/*".into()),
        ("user-agent".into(), agent(VERSION)),
        ("x-version".into(), VERSION.into()),
        ("x-tm".into(), platform().into()),
        ("x-product".into(), "autoclaw".into()),
        ("x-auth-appid".into(), APP_ID.into()),
        ("x-auth-timestamp".into(), stamp),
        ("x-auth-sign".into(), sign),
        ("x-trace-id".into(), random_hex(16)?),
        ("x-lang".into(), "zh-CN".into()),
        ("x-channel".into(), "AutoClaw4".into()),
        ("origin".into(), "http://localhost:18432".into()),
        ("referer".into(), "http://localhost:18432/".into()),
    ]);
    if !access.is_empty() {
        headers.insert(
            "authorization".into(),
            format!("Bearer {}", auth::bare(access)),
        );
    }
    Ok(headers)
}
pub(super) async fn send(
    client: &dyn HttpClient,
    request: HttpRequest,
) -> PluginResult<HttpStreamingResponse> {
    tokio::time::timeout(Duration::from_secs(30), client.stream(request))
        .await
        .map_err(|_| PluginError::transport("The AutoClaw request timed out."))?
        .map_err(|_| PluginError::transport("The AutoClaw connection stopped."))
}
pub(super) async fn bytes(
    response: HttpStreamingResponse,
    limit: usize,
) -> PluginResult<(u16, BTreeMap<String, String>, Vec<u8>)> {
    let status = response.status;
    let headers = response.headers;
    let body = tokio::time::timeout(
        Duration::from_secs(30),
        nora_ai::collect_http_body_text_limited(response.body, limit),
    )
    .await
    .map_err(|_| PluginError::transport("The AutoClaw reply timed out."))?
    .map_err(|_| PluginError::transport("AutoClaw returned an invalid or oversized reply."))?;
    Ok((status, headers, body.into_bytes()))
}
pub(super) struct Envelope {
    pub code: i64,
    pub data: Value,
}
pub(super) async fn call(
    client: &dyn HttpClient,
    host: &str,
    path: &str,
    method: HttpMethod,
    access: &str,
    body: Option<Value>,
) -> PluginResult<Envelope> {
    let request = HttpRequest {
        method,
        url: format!("{host}{path}"),
        headers: headers(access, now())?,
        body: body.map(|v| v.to_string().into_bytes()),
    };
    let (status, _, bytes) = bytes(send(client, request).await?, 4 * 1024 * 1024).await?;
    if matches!(status, 401 | 403) {
        return Err(signed_out());
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| PluginError::transport("AutoClaw returned invalid response JSON."))?;
    let code = number(&value["code"])
        .filter(|n| n.fract() == 0.0)
        .ok_or_else(|| PluginError::transport("AutoClaw returned no response code."))?
        as i64;
    if !(200..300).contains(&status) {
        return Err(PluginError::transport("AutoClaw declined the request."));
    }
    Ok(Envelope {
        code,
        data: value.get("data").cloned().unwrap_or(Value::Null),
    })
}
