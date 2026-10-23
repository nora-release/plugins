use crate::provider::*;
use crate::sign;
use nora_ai::{HttpClient, HttpMethod, HttpRequest};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

pub(super) struct Pending {
    pub deadline: u64,
    result: oneshot::Receiver<PluginResult<Value>>,
    task: tokio::task::AbortHandle,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(super) async fn authorize(provider: &CodeArts, params: Value) -> PluginResult<Value> {
    let method = params
        .get("method")
        .or_else(|| params.get("authMethodID"))
        .and_then(Value::as_str)
        .unwrap_or("oauth");
    if method != "oauth" {
        return Err(PluginError::unsupported(method));
    }
    let client = provider.params_client(&params)?;
    let (verifier, challenge) = sign::pkce()?;
    let key = sign::key();
    let ticket = sign::id(32)?;
    let session = sign::id(16)?;
    let listener = loop {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| invalid("Cannot open the Huawei Cloud sign-in callback."))?;
        if listener
            .local_addr()
            .map_err(|_| invalid("The callback address is unavailable."))?
            .port()
            >= 10000
        {
            break listener;
        }
    };
    let port = listener
        .local_addr()
        .map_err(|_| invalid("The callback address is unavailable."))?
        .port();
    let mut url = url::Url::parse("https://codearts.huaweicloud.com/portal/authorize").unwrap();
    url.query_pairs_mut()
        .append_pair("theme", "2")
        .append_pair("locale", "zh-cn")
        .append_pair("uri_scheme", "codearts-agent")
        .append_pair("client_id", "codearts-agent")
        .append_pair("port", &port.to_string())
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "SHA-256")
        .append_pair("ticket_id", &ticket)
        .append_pair("plugin-name", "snap_AIIDE")
        .append_pair("plugin-version", "5.2.0");
    let (sender, result) = oneshot::channel();
    let deadline = now() + 180_000;
    let task = tokio::spawn(async move {
        let result = tokio::time::timeout(
            Duration::from_secs(180),
            listen(listener, client, &verifier, &key, port),
        )
        .await
        .unwrap_or_else(|_| Err(invalid("The Huawei Cloud sign-in timed out.")));
        let _ = sender.send(result);
    });
    let mut pending = provider.pending.lock().await;
    pending.retain(|_, pending| pending.deadline > now());
    if pending.len() >= 32 {
        task.abort();
        return Err(invalid("Too many Huawei Cloud sign-ins are waiting."));
    }
    pending.insert(
        session.clone(),
        Pending {
            deadline,
            result,
            task: task.abort_handle(),
        },
    );
    Ok(
        json!({"session":session,"url":url.to_string(),"method":"auto","instructions":"Complete the Huawei Cloud sign-in in the browser."}),
    )
}

async fn listen(
    listener: tokio::net::TcpListener,
    client: Arc<dyn HttpClient>,
    verifier: &str,
    key: &Value,
    port: u16,
) -> PluginResult<Value> {
    loop {
        let (mut socket, _) = listener
            .accept()
            .await
            .map_err(|_| invalid("The Huawei callback stopped."))?;
        let mut request = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(5), async {
            let mut chunk = [0u8; 1024];
            loop {
                let count = socket.read(&mut chunk).await?;
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..count]);
                if request.len() > 8192 {
                    return Err(std::io::Error::other("The callback request is too large"));
                }
                if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    break;
                }
            }
            Ok::<(), std::io::Error>(())
        })
        .await;
        if !matches!(read, Ok(Ok(()))) {
            continue;
        }
        let request = String::from_utf8_lossy(&request);
        let mut first = request.lines().next().unwrap_or("").split_whitespace();
        let method = first.next().unwrap_or("");
        let path = first.next().unwrap_or("");
        let url = url::Url::parse(&format!("http://127.0.0.1:{port}{path}"));
        let code = url
            .as_ref()
            .ok()
            .filter(|url| url.path() == "/oauth/callback" && method == "GET")
            .and_then(|url| {
                url.query_pairs()
                    .find(|(key, _)| key == "code")
                    .map(|(_, code)| code.into_owned())
            })
            .filter(|code| !code.is_empty() && code.len() <= 4096);
        let Some(code) = code else {
            let _ = socket
                .write_all(
                    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            continue;
        };
        let token = request_token(
            client.as_ref(),
            key,
            BTreeMap::from([
                ("client_id", "codearts-agent".into()),
                ("code", code),
                ("code_verifier", verifier.into()),
                ("grant_type", "authorization_code".into()),
                (
                    "redirect_uri",
                    format!("http://127.0.0.1:{port}/oauth/callback"),
                ),
            ]),
        )
        .await;
        let succeeded = token.is_ok();
        let response=format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: https://codearts.huaweicloud.com/portal/login?login_succeed={succeeded}&uri_scheme=codearts-agent&locale=zh-cn\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let _ = socket.write_all(response.as_bytes()).await;
        return credential(token?, verifier, key, None);
    }
}

pub(super) async fn callback(provider: &CodeArts, params: Value) -> PluginResult<Value> {
    let mut pending = provider
        .pending
        .lock()
        .await
        .remove(text(&params, "session"))
        .ok_or_else(|| invalid("The Huawei Cloud sign-in expired."))?;
    let remaining = pending.deadline.saturating_sub(now());
    let credentials = tokio::time::timeout(Duration::from_millis(remaining), &mut pending.result)
        .await
        .map_err(|_| invalid("The Huawei Cloud sign-in timed out."))?
        .map_err(|_| invalid("The Huawei Cloud sign-in stopped."))??;
    Ok(json!({"ok":true,"credentials":encode(credentials)?}))
}

async fn request_token(
    client: &dyn HttpClient,
    key: &Value,
    fields: BTreeMap<&str, String>,
) -> PluginResult<Value> {
    let proof = sign::dpop(key, "POST", STS, now())?;
    let form = {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in fields {
            serializer.append_pair(key, &value);
        }
        serializer.finish()
    };
    let response = send(
        client,
        HttpRequest {
            method: HttpMethod::Post,
            url: STS.into(),
            headers: BTreeMap::from([
                ("dpop".into(), proof),
                (
                    "content-type".into(),
                    "application/x-www-form-urlencoded".into(),
                ),
            ]),
            body: Some(form.into_bytes()),
        },
        Duration::from_secs(60),
    )
    .await?;
    let (status, _, bytes) = body(response, 1024 * 1024).await?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| PluginError::transport("Huawei STS returned invalid token JSON."))?;
    if !(200..300).contains(&status) || !value.get("credentials").is_some_and(Value::is_object) {
        let raw = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
        if text(&value, "error") == "invalid_grant"
            || [
                "expiredrefreshtoken",
                "sts5.1806",
                "invalid refresh token",
                "refresh token has been used",
            ]
            .iter()
            .any(|word| raw.contains(word))
        {
            return Err(signed_out());
        }
        return Err(PluginError::transport(format!(
            "Huawei STS declined token renewal (HTTP {status})."
        )));
    }
    Ok(value)
}

pub(super) fn credential(
    token: Value,
    verifier: &str,
    key: &Value,
    previous: Option<&Value>,
) -> PluginResult<Value> {
    let raw = token.get("credentials").ok_or_else(signed_out)?;
    let mut value = json!({"access_key_id":raw.get("access_key_id"),"secret_access_key":raw.get("secret_access_key"),"security_token":raw.get("security_token"),"expires_at":raw.get("expiration"),"refresh_token":token.get("refresh_token"),"code_verifier":verifier,"dpop_private_key_jwk":key,"session_id":sign::id(16)?});
    for field in ["access_key_id", "secret_access_key", "security_token"] {
        if text(&value, field).is_empty() {
            return Err(invalid(
                "Huawei STS returned incomplete account credentials.",
            ));
        }
    }
    if let Some(previous) = previous {
        if text(&value, "refresh_token").is_empty() {
            value["refresh_token"] = previous
                .get("refresh_token")
                .cloned()
                .unwrap_or(Value::Null);
        }
        for field in ["domain_id", "user_id", "user_name", "session_id"] {
            if !text(previous, field).is_empty() {
                value[field] = previous[field].clone();
            }
        }
    }
    // Validate actual expiration now, rather than inventing another day's access.
    encode(value.clone())?;
    Ok(value)
}

pub(super) async fn renew(
    provider: &CodeArts,
    client: &dyn HttpClient,
    previous: &Value,
) -> PluginResult<Value> {
    let refresh = text(previous, "refresh_token");
    let verifier = text(previous, "code_verifier");
    let key = previous
        .get("dpop_private_key_jwk")
        .ok_or_else(signed_out)?;
    if refresh.is_empty() || verifier.is_empty() {
        return Err(signed_out());
    }
    let id = sign::digest(refresh.as_bytes());
    let mut cached = provider.refreshing.lock().await;
    cached.retain(|_, (_, at)| now().saturating_sub(*at) < 3_600_000);
    if let Some((value, _)) = cached.get(&id) {
        return Ok(value.clone());
    }
    let token = request_token(
        client,
        key,
        BTreeMap::from([
            ("client_id", "codearts-agent".into()),
            ("code_verifier", verifier.into()),
            ("grant_type", "refresh_token".into()),
            ("refresh_token", refresh.into()),
        ]),
    )
    .await?;
    let value = credential(token, verifier, key, Some(previous))?;
    if cached.len() >= 128 {
        if let Some(first) = cached
            .iter()
            .min_by_key(|(_, (_, at))| *at)
            .map(|(key, _)| key.clone())
        {
            cached.remove(&first);
        }
    }
    cached.insert(id, (value.clone(), now()));
    Ok(value)
}

#[cfg(test)]
#[path = "../test/auth.test.rs"]
mod tests;
