use crate::*;
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use p256::{
    ecdsa::{signature::Signer, Signature, SigningKey},
    pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey, LineEnding},
    SecretKey,
};
use rand::Rng;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::oneshot,
};
pub(crate) struct Pending {
    deadline: u64,
    result: oneshot::Receiver<PluginResult<Value>>,
    task: tokio::task::AbortHandle,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub(crate) fn keys() -> PluginResult<(String, String)> {
    let secret = SecretKey::random(&mut rand::rngs::OsRng);
    let private = secret
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|_| invalid("The Trae device key could not be created."))?;
    let public = secret
        .public_key()
        .to_public_key_pem(LineEnding::LF)
        .map_err(|_| invalid("The Trae device key could not be created."))?;
    Ok((public, private.to_string()))
}
fn device(site: Site) -> PluginResult<Value> {
    let count = if site == Site::China { 16 } else { 19 };
    let mut rng = rand::rngs::OsRng;
    let device = (0..count)
        .map(|index| char::from(b'0' + rng.gen_range(if index == 0 { 1..10 } else { 0..10 })))
        .collect::<String>();
    let (public, private) = keys()?;
    Ok(
        json!({"deviceId":device,"machineId":id(),"devicePublicKey":public,"devicePrivateKey":private}),
    )
}
pub(crate) fn proof(
    path: &str,
    client: &str,
    refresh: &str,
    private: &str,
    at: u64,
    nonce: &str,
) -> PluginResult<Value> {
    let key = SigningKey::from_pkcs8_pem(private)
        .map_err(|_| invalid("The Trae device sign-in key is invalid. Sign in again."))?;
    let input = format!("POST\n{path}\n{client}\n{refresh}\n{at}\n{nonce}");
    let signature: Signature = key.sign(input.as_bytes());
    Ok(
        json!({"Signature":STANDARD.encode(signature.to_der().as_bytes()),"Timestamp":at,"Nonce":nonce}),
    )
}
pub(crate) async fn authorize(provider: &Trae, params: Value) -> PluginResult<Value> {
    let method = params
        .get("authMethodID")
        .or_else(|| params.get("method"))
        .and_then(Value::as_str)
        .unwrap_or("oauth");
    if method != "oauth" && !(method == "oauth-client" && provider.site == Site::China) {
        return Err(PluginError::unsupported(method));
    }
    let input = params
        .get("inputs")
        .or_else(|| params.get("fields"))
        .unwrap_or(&params);
    let desktop = (method == "oauth-client").then(|| text(input, "path").to_owned());
    let client = provider.params_client(&params)?;
    let device = device(provider.site)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| invalid("The Trae sign-in callback could not be opened."))?;
    let port = listener
        .local_addr()
        .map_err(|_| invalid("The Trae callback address is unavailable."))?
        .port();
    let callback = format!("http://127.0.0.1:{port}/authorize");
    let mut url = url::Url::parse(&format!("{}/authorization", provider.site.web())).unwrap();
    for (key, value) in [
        ("login_version", "1".into()),
        ("auth_from", "solo".into()),
        ("login_channel", "native_ide".into()),
        ("plugin_version", provider.site.plugin().into()),
        ("auth_type", "local".into()),
        ("client_id", site::CLIENT.into()),
        ("redirect", "0".into()),
        ("login_trace_id", uuid::Uuid::new_v4().to_string()),
        ("auth_callback_url", callback),
        ("machine_id", text(&device, "machineId").into()),
        ("device_id", text(&device, "deviceId").into()),
        ("x_device_id", text(&device, "deviceId").into()),
        ("x_machine_id", text(&device, "machineId").into()),
        ("x_device_brand", site::BRAND.into()),
        ("x_device_type", "windows".into()),
        ("x_os_version", "Windows 10 Pro".into()),
        ("x_env", String::new()),
        ("x_app_version", provider.site.ide().into()),
        ("x_app_type", "stable".into()),
        ("hide_saas_login", "false".into()),
    ] {
        url.query_pairs_mut().append_pair(key, &value);
    }
    let (sender, result) = oneshot::channel();
    let site = provider.site;
    let task = tokio::spawn(async move {
        let result = tokio::time::timeout(
            Duration::from_secs(600),
            listen(listener, client, site, device, desktop),
        )
        .await
        .unwrap_or_else(|_| Err(invalid("The Trae sign-in timed out.")));
        let _ = sender.send(result);
    });
    let session = id();
    let mut pending = provider.pending.lock().await;
    pending.retain(|_, flow| flow.deadline > now());
    if pending.len() >= 32 {
        task.abort();
        return Err(invalid("Too many Trae sign-ins are waiting."));
    }
    pending.insert(
        session.clone(),
        Pending {
            deadline: now() + 600_000,
            result,
            task: task.abort_handle(),
        },
    );
    Ok(
        json!({"session":session,"url":url.to_string(),"method":"auto","instructions":"Sign in to Trae in your browser and allow the sign-in."}),
    )
}
async fn listen(
    listener: tokio::net::TcpListener,
    client: Arc<dyn HttpClient>,
    site: Site,
    device: Value,
    desktop: Option<String>,
) -> PluginResult<Value> {
    loop {
        let (mut socket, _) = listener
            .accept()
            .await
            .map_err(|_| invalid("The Trae callback stopped."))?;
        let mut bytes = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(5), async {
            let mut buffer = [0u8; 2048];
            loop {
                let size = socket.read(&mut buffer).await?;
                if size == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..size]);
                if bytes.len() > 65536 {
                    return Err(std::io::Error::other("Callback too large"));
                }
                if bytes.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    break;
                }
            }
            Ok::<_, std::io::Error>(())
        })
        .await;
        if !matches!(read, Ok(Ok(()))) {
            continue;
        }
        let raw = String::from_utf8_lossy(&bytes);
        let mut first = raw.lines().next().unwrap_or("").split_whitespace();
        let method = first.next().unwrap_or("");
        let target = first.next().unwrap_or("");
        if method == "OPTIONS" {
            let _=socket.write_all(b"HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\nConnection: close\r\n\r\n").await;
            continue;
        }
        let url = url::Url::parse(&format!("http://127.0.0.1{target}"));
        let Ok(url) = url else { continue };
        if method != "GET" || url.path() != "/authorize" {
            let _ = socket
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            continue;
        }
        let query = url.query_pairs().into_owned().collect::<BTreeMap<_, _>>();
        if !query.contains_key("userJwt")
            && !query.contains_key("refreshToken")
            && !query.contains_key("error")
        {
            let _=socket.write_all(b"HTTP/1.1 400 Bad Request\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
            continue;
        }
        let result = signed_in(client.as_ref(), site, &query, &device, desktop.as_deref()).await;
        let message = if result.is_ok() {
            "Trae sign-in completed. You can close this tab."
        } else {
            "Trae sign-in did not finish. Return to Nora to see the error."
        };
        let response=format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{message}",message.len());
        let _ = socket.write_all(response.as_bytes()).await;
        return result;
    }
}
pub(crate) async fn callback(provider: &Trae, params: Value) -> PluginResult<Value> {
    let mut pending = provider
        .pending
        .lock()
        .await
        .remove(text(&params, "session"))
        .ok_or_else(|| invalid("The Trae sign-in expired. Start again."))?;
    let result = tokio::time::timeout(
        Duration::from_millis(pending.deadline.saturating_sub(now())),
        &mut pending.result,
    )
    .await
    .map_err(|_| invalid("The Trae sign-in timed out."))?
    .map_err(|_| invalid("The Trae sign-in stopped."))??;
    Ok(json!({"ok":true,"credentials":result}))
}
fn parsed(value: &Value) -> Value {
    value
        .as_str()
        .and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or_else(|| value.clone())
}
fn field<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter()
        .find_map(|key| value.get(*key).filter(|value| !value.is_null()))
}
fn jwt_exp(token: &str) -> Option<u64> {
    let payload = token.split('.').nth(1)?;
    let value: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
    value.get("exp").and_then(usage::date)
}
fn region(info: &Value) -> String {
    field(info, &["region", "aiRegion", "Region", "AIRegion"])
        .map(identifier)
        .unwrap_or_default()
}
fn account(info: &Value) -> Value {
    let mut account = serde_json::Map::new();
    if let Some(info) = info.as_object() {
        for (key, value) in info {
            let lower = key.to_ascii_lowercase();
            if [
                "tenant",
                "enterprise",
                "saas",
                "login",
                "org",
                "team",
                "sso",
                "usertype",
                "accounttype",
            ]
            .iter()
            .any(|word| lower.contains(word))
                || lower.starts_with("ent")
            {
                if ![
                    "token", "secret", "key", "password", "cookie", "session", "jwt",
                ]
                .iter()
                .any(|word| lower.contains(word))
                    && (value.is_null()
                        || value.is_string()
                        || value.is_boolean()
                        || value.is_number())
                {
                    account.insert(key.clone(), value.clone());
                }
            }
        }
    }
    Value::Object(account)
}
fn normalized(value: &Value, refresh: &str, client: &str) -> PluginResult<Value> {
    let raw = value
        .get("Result")
        .or_else(|| value.get("result"))
        .unwrap_or(value);
    let token = field(raw, &["Token", "token"])
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| transport("Trae did not return an access token."))?;
    let info = field(raw, &["UserInfo", "userInfo"])
        .map(parsed)
        .unwrap_or(Value::Null);
    let uid = field(&info, &["UserID", "userId"])
        .or_else(|| raw.get("UserID"))
        .map(identifier)
        .unwrap_or_default();
    let expires = field(raw, &["TokenExpireAt", "tokenExpireAt"])
        .and_then(usage::date)
        .or_else(|| jwt_exp(token))
        .unwrap_or_else(|| now() + 86_400_000);
    Ok(
        json!({"type":"oauth","access":token,"refresh":field(raw,&["RefreshToken","refreshToken"]).and_then(Value::as_str).filter(|value|!value.is_empty()).unwrap_or(refresh),"expires":expires,"clientId":field(raw,&["ClientID","clientId"]).and_then(Value::as_str).unwrap_or(client),"uid":uid,"accountId":field(&info,&["ScreenName","screenName"]).and_then(Value::as_str).unwrap_or(""),"email":field(&info,&["Email","email"]).and_then(Value::as_str).unwrap_or(""),"api":text(&info,"Host"),"region":region(&info),"account":account(&info)}),
    )
}
async fn signed_in(
    client: &dyn HttpClient,
    site: Site,
    query: &BTreeMap<String, String>,
    device: &Value,
    desktop: Option<&str>,
) -> PluginResult<Value> {
    let q = |key: &str| query.get(key).map(String::as_str).unwrap_or("");
    if !q("error").is_empty() {
        return Err(invalid("Trae did not authorize this sign-in."));
    }
    let jwt: Value = serde_json::from_str(q("userJwt")).unwrap_or(json!({}));
    let info: Value = serde_json::from_str(q("userInfo")).unwrap_or(json!({}));
    let refresh = field(&jwt, &["RefreshToken", "refreshToken"])
        .and_then(Value::as_str)
        .unwrap_or_else(|| q("refreshToken"));
    let client_id = field(&jwt, &["ClientID", "clientId"])
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            if q("clientId").is_empty() {
                site::CLIENT
            } else {
                q("clientId")
            }
        });
    let mut value=normalized(&json!({"Result":{"Token":field(&jwt,&["Token","token"]).cloned().unwrap_or(json!("")),"RefreshToken":refresh,"TokenExpireAt":field(&jwt,&["TokenExpireAt","tokenExpireAt"]),"ClientID":client_id,"UserInfo":info}}),refresh,client_id).unwrap_or_else(|_|json!({"refresh":refresh,"clientId":client_id}));
    if text(&value, "uid").is_empty() {
        value["uid"] = json!(field(&info, &["UserID", "userId"])
            .map(identifier)
            .unwrap_or_default());
    }
    if text(&value, "accountId").is_empty() {
        value["accountId"] = json!(field(&info, &["ScreenName", "screenName"])
            .and_then(Value::as_str)
            .unwrap_or(""));
    }
    value["region"] = json!(region(&info));
    value["account"] = account(&info);
    if text(&value, "email").is_empty() {
        value["email"] = json!(field(&info, &["Email", "email"])
            .and_then(Value::as_str)
            .unwrap_or(""));
    }
    let host = if q("host").is_empty() {
        text(&info, "Host")
    } else {
        q("host")
    };
    if let Some(host) = site::host(host) {
        value["host"] = json!(host);
        value["api"] = json!(host);
    }
    let auth_host = if !q("authHost").is_empty() {
        q("authHost")
    } else if !q("auth_host").is_empty() {
        q("auth_host")
    } else {
        field(&info, &["AuthHost", "authHost"])
            .and_then(Value::as_str)
            .unwrap_or("")
    };
    if !auth_host.is_empty() {
        value["authHost"] = json!(auth_host);
        value["authHost"] = json!(site.auth_host(&value));
    }
    if text(&value, "access").is_empty() && !refresh.is_empty() {
        let exchanged = exchange(client, site, &value).await?;
        let original = value.clone();
        value = exchanged;
        for field in [
            "host",
            "authHost",
            "api",
            "uid",
            "accountId",
            "email",
            "region",
            "account",
        ] {
            if original.get(field).is_some()
                && (matches!(field, "host" | "authHost" | "account")
                    || text(&value, field).is_empty())
            {
                value[field] = original[field].clone();
            }
        }
    }
    if text(&value, "uid").is_empty() {
        value["uid"] = json!(field(&info, &["UserID", "userId"])
            .map(identifier)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| q("userId").to_owned()));
    }
    if text(&value, "accountId").is_empty() {
        value["accountId"] = json!(if !text(&info, "ScreenName").is_empty() {
            text(&info, "ScreenName")
        } else {
            text(&value, "uid")
        });
    }
    for (key, field) in device.as_object().into_iter().flatten() {
        value[key] = field.clone();
    }
    if text(&value, "access").is_empty() || text(&value, "refresh").is_empty() {
        return Err(signed_out());
    }
    if site == Site::China {
        if let Ok(bound) = bound(client, site, &value).await {
            for key in [
                "access",
                "refresh",
                "expires",
                "devicePublicKey",
                "devicePrivateKey",
            ] {
                if bound.get(key).is_some() {
                    value[key] = bound[key].clone();
                }
            }
        }
    }
    if let Some(path) = desktop {
        desktop::mark(&mut value, path).await?;
    }
    credentials(&value)
}
async fn exchange(client: &dyn HttpClient, site: Site, credentials: &Value) -> PluginResult<Value> {
    let refresh = text(credentials, "refresh");
    if refresh.is_empty() {
        return Err(signed_out());
    }
    let client_id = if text(credentials, "clientId").is_empty() {
        site::CLIENT
    } else {
        text(credentials, "clientId")
    };
    let (status, value) = json_request(
        client,
        HttpRequest {
            method: HttpMethod::Post,
            url: format!(
                "{}/cloudide/api/v3/trae/oauth/ExchangeToken",
                site.auth_host(credentials)
            ),
            headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
            body: Some(
                json!({"ClientID":client_id,"RefreshToken":refresh,"ClientSecret":"-","UserID":""})
                    .to_string()
                    .into_bytes(),
            ),
        },
    )
    .await?;
    let (code, message) = error_of(&value);
    if (200..300).contains(&status) {
        if let Ok(value) = normalized(&value, refresh, client_id) {
            return Ok(value);
        }
    }
    let lower = message.to_ascii_lowercase();
    if [400, 401, 403].contains(&status)
        || code == "10101"
        || [
            "refresh",
            "expired",
            "invalid",
            "not matched",
            "revoked",
            "unauthori",
        ]
        .iter()
        .any(|word| lower.contains(word))
    {
        return Err(signed_out());
    }
    Err(transport(format!(
        "Trae token renewal failed (HTTP {status})."
    )))
}
async fn bound(client: &dyn HttpClient, site: Site, credentials: &Value) -> PluginResult<Value> {
    let path = "/trae/api/v3/oauth/ExchangeToken";
    let mut credentials = credentials.clone();
    if text(&credentials, "devicePrivateKey").is_empty()
        || text(&credentials, "devicePublicKey").is_empty()
    {
        let (public, private) = keys()?;
        credentials["devicePublicKey"] = json!(public);
        credentials["devicePrivateKey"] = json!(private);
    }
    let mut last = transport("Trae could not renew the device-bound sign-in.");
    for (client_id, platform, version) in [
        ("en1oxy7wnw8j9n", "SOLO_PC", "0.1.64"),
        (site::CLIENT, "IDE_PC", "3.3.99"),
    ] {
        let proof = proof(
            path,
            client_id,
            text(&credentials, "refresh"),
            text(&credentials, "devicePrivateKey"),
            now() / 1000,
            &id(),
        )?;
        let body = json!({"ClientID":client_id,"ClientSecret":"","RefreshToken":credentials.get("refresh"),"DeviceInfo":{"DeviceID":credentials.get("deviceId"),"MachineID":credentials.get("machineId"),"PlatformCode":platform,"DeviceType":"PC","DeviceName":"","DeviceModel":"","ClientVersion":version,"DevicePublicKey":credentials.get("devicePublicKey"),"DeviceBrand":site::BRAND,"DeviceCPU":"AMD","OSInfo":"Windows","OSVersion":"10.0.22631"},"DeviceProof":proof,"IDEVersion":version});
        let mut headers = BTreeMap::from([("content-type".into(), "application/json".into())]);
        if !text(&credentials, "access").is_empty() {
            headers.insert(
                "authorization".into(),
                format!("Cloud-IDE-JWT {}", text(&credentials, "access")),
            );
        }
        match json_request(
            client,
            HttpRequest {
                method: HttpMethod::Post,
                url: format!("{}{path}", site.auth_host(&credentials)),
                headers,
                body: Some(body.to_string().into_bytes()),
            },
        )
        .await
        {
            Ok((status, value)) if (200..300).contains(&status) => {
                if let Ok(mut next) = normalized(
                    &value,
                    text(&credentials, "refresh"),
                    text(&credentials, "clientId"),
                ) {
                    next["devicePublicKey"] = credentials["devicePublicKey"].clone();
                    next["devicePrivateKey"] = credentials["devicePrivateKey"].clone();
                    next.as_object_mut().unwrap().remove("clientId");
                    return Ok(next);
                }
                last = transport("Trae device renewal returned no access token.");
            }
            Ok((status, _)) => {
                last = transport(format!("Trae device renewal failed (HTTP {status})."))
            }
            Err(error) => last = error,
        }
    }
    Err(last)
}
pub(crate) async fn renew(
    provider: &Trae,
    client: &dyn HttpClient,
    previous: &Value,
) -> PluginResult<Value> {
    let key = format!(
        "{:x}",
        Sha256::digest(
            format!(
                "{}\0{}\0{}",
                provider.site.auth_host(previous),
                text(previous, "refresh"),
                text(previous, "access")
            )
            .as_bytes()
        )
    );
    let mut cache = provider.renewed.lock().await;
    cache.retain(|_, (_, at)| now().saturating_sub(*at) < 3_600_000);
    if let Some((next, _)) = cache.get(&key) {
        return Ok(next.clone());
    }
    let mut prepared = previous.clone();
    if provider.site == Site::China
        && (text(previous, "devicePrivateKey").is_empty()
            || text(previous, "devicePublicKey").is_empty())
    {
        let (public, private) = keys()?;
        prepared["devicePublicKey"] = json!(public);
        prepared["devicePrivateKey"] = json!(private);
    }
    let refreshed = if provider.site == Site::China {
        match bound(client, provider.site, &prepared).await {
            Ok(value) => value,
            Err(_) => exchange(client, provider.site, &prepared).await?,
        }
    } else {
        exchange(client, provider.site, &prepared).await?
    };
    let mut next = prepared;
    for field in [
        "access",
        "refresh",
        "expires",
        "clientId",
        "devicePublicKey",
        "devicePrivateKey",
    ] {
        if refreshed.get(field).is_some() {
            next[field] = refreshed[field].clone();
        }
    }
    if cache.len() >= 128 {
        if let Some(old) = cache
            .iter()
            .min_by_key(|(_, (_, at))| at)
            .map(|(key, _)| key.clone())
        {
            cache.remove(&old);
        }
    }
    cache.insert(key, (next.clone(), now()));
    Ok(next)
}
#[cfg(test)]
#[path = "../test/auth.test.rs"]
mod tests;
