use crate::{
    cookies::{self, Jar},
    net::{self, Net, Reply, Request},
};
use nora_provider_plugin_sdk::{NetworkProxy, PluginError, PluginResult};
use nora_provider_subscription_core as core;
use rand::RngCore;
use serde_json::{json, Value};
use std::{collections::BTreeMap, time::Duration};
pub(crate) const ACCOUNT: &str = "https://account.xiaomi.com";
pub(crate) const PLATFORM: &str = "https://platform.xiaomimimo.com";
pub(crate) const VERSION: &str = "26.929.292248";
pub(crate) const UA:&str="miNative PC/Normal Windows_NT/10.0.26100 SDKV/1.0.0 DEVT/PC DEVS/Windows APP/miaccount_desktop APPV/0.1.0";
pub(crate) fn base(region: &str) -> Option<&'static str> {
    match region.trim().to_ascii_uppercase().as_str() {
        "SGP" => Some("https://mimo-server-sgp.xiaomimimo.com/api"),
        "RU" => Some("https://mimo-server-ru.xiaomimimo.com/api"),
        "IN" => Some("https://mimo-server-in.xiaomimimo.com/api"),
        _ => None,
    }
}
pub(crate) fn app_headers() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("user-agent".into(), UA.into()),
        ("x-client-version".into(), VERSION.into()),
    ])
}
pub(crate) fn id(value: &Value) -> Option<String> {
    let value = value.as_str().map(str::to_owned).or_else(|| {
        if let Value::Number(number) = value {
            Some(number.to_string())
        } else {
            None
        }
    })?;
    if value.is_empty() || value.len() > 2048 || value.chars().any(char::is_control) {
        None
    } else {
        Some(value)
    }
}
pub(crate) fn account_json(bytes: &[u8]) -> Option<Value> {
    let text = std::str::from_utf8(bytes)
        .ok()?
        .trim()
        .strip_prefix("&&&START&&&")
        .unwrap_or(std::str::from_utf8(bytes).ok()?.trim());
    serde_json::from_str(text).ok()
}
pub(crate) fn data(bytes: &[u8]) -> Option<Value> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    if value["code"] == 0 {
        Some(value.get("data").cloned().unwrap_or(Value::Null))
    } else {
        None
    }
}
fn header(headers: &BTreeMap<String, String>, key: &str) -> Option<String> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, value)| value.to_owned())
}
fn account_url(raw: &str) -> PluginResult<url::Url> {
    let url = url::Url::parse(raw)
        .map_err(|_| PluginError::authentication("Xiaomi returned an invalid sign-in address."))?;
    if url.scheme() != "https"
        || url.host_str() != Some("account.xiaomi.com")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(PluginError::authentication(
            "Xiaomi's sign-in requires its official HTTPS account page.",
        ));
    }
    Ok(url)
}
fn seed(creds: &Value) -> PluginResult<Jar> {
    let mut jar = Jar::default();
    for (key, value) in [
        ("userId", core::required(creds, "userId")?),
        ("passToken", core::required(creds, "passToken")?),
        ("cUserId", core::text(&creds["cUserId"]).unwrap_or("")),
        ("deviceId", core::required(creds, "deviceId")?),
        ("pass_ua", "pc"),
        ("uLocale", "zh_CN"),
    ] {
        if !value.is_empty() {
            jar.seed(ACCOUNT, key, value)?;
        }
    }
    Ok(jar)
}
fn redirect(current: &str, location: &str, target: &str) -> PluginResult<String> {
    let mut next = url::Url::parse(current)
        .map_err(|_| PluginError::transport("Invalid Xiaomi redirect source."))?
        .join(location)
        .map_err(|_| PluginError::transport("Xiaomi returned an invalid redirect."))?;
    let target = url::Url::parse(target).unwrap();
    if next.scheme() == "http" && next.host_str() == target.host_str() {
        next.set_scheme("https")
            .map_err(|_| PluginError::transport("Invalid Xiaomi redirect scheme."))?;
    }
    let allowed = next.host_str().is_some_and(|host| {
        [
            "account.xiaomi.com",
            "platform.xiaomimimo.com",
            "mimo-server-sgp.xiaomimimo.com",
            "mimo-server-ru.xiaomimimo.com",
            "mimo-server-in.xiaomimimo.com",
        ]
        .contains(&host)
    });
    if next.scheme() != "https"
        || !allowed
        || next.port_or_known_default() != Some(443)
        || !next.username().is_empty()
        || next.password().is_some()
        || next.fragment().is_some()
    {
        return Err(PluginError::transport(
            "Xiaomi's sign-in redirected outside its supported HTTPS services.",
        ));
    }
    Ok(next.to_string())
}
async fn follow(
    net: &dyn Net,
    creds: &Value,
    initial: &str,
    target: &str,
    app: bool,
    proxy: Option<&NetworkProxy>,
) -> PluginResult<(Reply, Jar, String)> {
    let mut jar = seed(creds)?;
    let mut at = redirect(initial, initial, target)?;
    for _ in 0..=10 {
        let mut headers = if app { app_headers() } else { BTreeMap::new() };
        let cookie = jar.header(&at)?;
        if !cookie.is_empty() {
            headers.insert("cookie".into(), cookie);
        }
        let reply = net
            .send(Request::get(&at, headers), proxy, Duration::from_secs(30))
            .await?;
        jar.take(&at, &reply.set_cookies)?;
        if (300..400).contains(&reply.status) {
            if let Some(location) = header(&reply.headers, "location") {
                at = redirect(&at, &location, target)?;
                drop(reply);
                continue;
            }
        }
        return Ok((reply, jar, at));
    }
    Err(PluginError::transport(
        "Xiaomi's sign-in exceeded ten redirects.",
    ))
}
pub(crate) struct Session {
    pub cookies: BTreeMap<String, String>,
    pub me: Value,
}
pub(crate) async fn session(
    net: &dyn Net,
    creds: &Value,
    region: &str,
    proxy: Option<&NetworkProxy>,
) -> PluginResult<Session> {
    let base =
        base(region).ok_or_else(|| PluginError::invalid("Unsupported Xiaomi MiMo region."))?;
    tokio::time::timeout(Duration::from_secs(30), async {
        let (reply, jar, at) = follow(
            net,
            creds,
            &format!("{base}/user/xiaomi/me"),
            base,
            true,
            proxy,
        )
        .await?;
        let (status, _, _, bytes) = net::collect(reply, 16 * 1024 * 1024).await?;
        if status == 429 || status >= 500 {
            return Err(PluginError::transport(
                "Xiaomi MiMo could not renew the session. Try again later.",
            ));
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
            PluginError::authentication("The Xiaomi sign-in has lapsed. Sign in again.")
        })?;
        if status == 403 || value["code"] == 403 || value["code"] == 46109 {
            return Err(PluginError::transport(
                "Xiaomi MiMo does not serve this account's region.",
            ));
        }
        if status != 200 || value["code"] != 0 || id(&value["data"]["userId"]).is_none() {
            return Err(PluginError::authentication(
                "The Xiaomi sign-in has lapsed. Sign in again.",
            ));
        }
        let uid = id(&value["data"]["userId"]).unwrap();
        if uid != core::required(creds, "userId")? {
            return Err(PluginError::authentication(
                "Xiaomi returned a different account while signing in. Reconnect it explicitly.",
            ));
        }
        if url::Url::parse(&at)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            != url::Url::parse(base)
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
        {
            return Err(PluginError::authentication(
                "Xiaomi did not finish signing in at the MiMo server.",
            ));
        }
        let cookies = jar.map(base)?;
        if cookies.is_empty() {
            return Err(PluginError::transport(
                "Xiaomi MiMo set no session cookies.",
            ));
        }
        Ok(Session {
            cookies,
            me: value["data"].clone(),
        })
    })
    .await
    .map_err(|_| PluginError::transport("Xiaomi MiMo's sign-in timed out."))?
}
pub(crate) struct Login {
    pub url: String,
    pub poll: String,
    pub device: String,
    pub until: u64,
}
async fn account_read(
    net: &dyn Net,
    url: &str,
    device: &str,
    proxy: Option<&NetworkProxy>,
    timeout: Duration,
) -> PluginResult<(u16, Vec<u8>)> {
    account_url(url)?;
    let headers = BTreeMap::from([
        ("user-agent".into(), UA.into()),
        (
            "cookie".into(),
            format!("deviceId={device}; pass_ua=pc; uLocale=zh_CN"),
        ),
    ]);
    let (status, _, _, bytes) = net::read(net, Request::get(url, headers), proxy, timeout).await?;
    Ok((status, bytes))
}
pub(crate) async fn login(net: &dyn Net, proxy: Option<&NetworkProxy>) -> PluginResult<Login> {
    let base = base("SGP").unwrap();
    let mut random = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut random);
    let device = format!("pc_{}", hex::encode(random));
    let (mut sid, mut callback) = ("mimosgp".to_owned(), format!("{base}/sts"));
    if let Ok(reply) = net
        .send(
            Request::get(format!("{base}/user/xiaomi/me"), app_headers()),
            proxy,
            Duration::from_secs(15),
        )
        .await
    {
        if let Some(location) = header(&reply.headers, "location") {
            if let Ok(url) = url::Url::parse(base)
                .unwrap()
                .join(&location)
                .and_then(|url| {
                    account_url(url.as_str()).map_err(|_| url::ParseError::RelativeUrlWithoutBase)
                })
            {
                if let Some(value) = url
                    .query_pairs()
                    .find(|(key, _)| key == "sid")
                    .map(|(_, value)| value.into_owned())
                    .filter(|value| !value.is_empty() && value.len() < 1024)
                {
                    sid = value;
                }
                if let Some(value) = url
                    .query_pairs()
                    .find(|(key, _)| key == "callback")
                    .map(|(_, value)| value.into_owned())
                {
                    if let Ok(value) = redirect(base, &value, base) {
                        if url::Url::parse(&value).unwrap().host_str()
                            == url::Url::parse(base).unwrap().host_str()
                        {
                            callback = value;
                        }
                    }
                }
            }
        }
    }
    let mut url = url::Url::parse(&format!("{ACCOUNT}/longPolling/loginUrl")).unwrap();
    url.query_pairs_mut()
        .append_pair("_group", "DEFAULT")
        .append_pair("_qrsize", "240")
        .append_pair(
            "qs",
            &format!("%3Fsid%3D{}%26_json%3Dtrue", core::encode(&sid)),
        )
        .append_pair("callback", &callback)
        .append_pair("_hasLogo", "false")
        .append_pair("sid", &sid)
        .append_pair("serviceParam", "")
        .append_pair("_locale", "en_US");
    let (status, bytes) =
        account_read(net, url.as_str(), &device, proxy, Duration::from_secs(20)).await?;
    let value = account_json(&bytes)
        .ok_or_else(|| PluginError::authentication("Xiaomi returned an invalid sign-in ticket."))?;
    if status != 200 || value["code"] != 0 {
        return Err(PluginError::authentication(
            "Xiaomi did not issue a sign-in ticket.",
        ));
    }
    let login = account_url(core::required(&value, "loginUrl")?)?;
    let poll = account_url(core::required(&value, "lp")?)?;
    let life = value["timeout"]
        .as_u64()
        .filter(|life| *life > 0)
        .unwrap_or(600)
        .min(600);
    Ok(Login {
        url: login.to_string(),
        poll: poll.to_string(),
        device,
        until: core::now() + life * 1000,
    })
}
pub(crate) async fn poll(
    net: &dyn Net,
    ticket: &Value,
    proxy: Option<&NetworkProxy>,
) -> PluginResult<Value> {
    let until = ticket["until"].as_u64().unwrap_or(0);
    let url = core::required(ticket, "poll")?;
    let device = core::required(ticket, "device")?;
    while core::now() < until {
        let timeout = Duration::from_millis(until.saturating_sub(core::now()).clamp(1, 70_000));
        if let Ok((200, bytes)) = account_read(net, url, device, proxy, timeout).await {
            if let Some(value) = account_json(&bytes) {
                if core::text(&value["passToken"]).is_some() && id(&value["userId"]).is_some() {
                    return Ok(value);
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(
            1000.min(until.saturating_sub(core::now())),
        ))
        .await;
    }
    Err(PluginError::authentication(
        "The Xiaomi sign-in page expired. Start again.",
    ))
}
pub(crate) async fn signed_in(
    net: &dyn Net,
    value: &Value,
    device: &str,
    proxy: Option<&NetworkProxy>,
) -> PluginResult<Value> {
    let uid = id(&value["userId"])
        .ok_or_else(|| PluginError::authentication("Xiaomi returned no account ID."))?;
    let mut creds = json!({"userId":uid,"cUserId":core::text(&value["cUserId"]).unwrap_or(""),"passToken":core::required(value,"passToken")?,"deviceId":device,"region":"SGP"});
    let mut session = session(net, &creds, "SGP", proxy).await?;
    if let Some(region) = session.me["region"]
        .as_str()
        .map(|region| region.trim().to_ascii_uppercase())
        .filter(|region| region != "SGP" && base(region).is_some())
    {
        if let Ok(regional) = self::session(net, &creds, &region, proxy).await {
            session = regional;
            creds["region"] = json!(region);
        }
    }
    Ok(credential(creds, session.cookies, &session.me))
}
pub(crate) fn credential(creds: Value, cookies: BTreeMap<String, String>, me: &Value) -> Value {
    json!({"provider":"mimo-app","type":"oauth","authMethod":"oauth","access":json!(cookies).to_string(),"refresh":creds.to_string(),"expires":core::now()+86_400_000,"uid":creds["userId"],"accountId":creds["userId"],"label":core::text(&me["nickname"]).or_else(||core::text(&me["name"])).unwrap_or("Xiaomi MiMo"),"xiaomi":creds,"cookies":cookies})
}
pub(crate) async fn platform_page(
    net: &dyn Net,
    creds: &Value,
    path: &str,
    cached: Option<&BTreeMap<String, String>>,
    proxy: Option<&NetworkProxy>,
) -> PluginResult<(BTreeMap<String, String>, Value)> {
    if ![
        "/tokenPlan/detail",
        "/tokenPlan/usage",
        "/tokenPlan/apiKey",
        "/tokenPlan/apiKey/raw",
    ]
    .contains(&path)
    {
        return Err(PluginError::invalid("Unsupported MiMo platform page."));
    }
    let url = format!("{PLATFORM}/api/v1{path}");
    tokio::time::timeout(Duration::from_secs(20), async {
        if let Some(cached) = cached.filter(|cookies| !cookies.is_empty()) {
            let headers = BTreeMap::from([("cookie".into(), cookies::header(cached)?)]);
            let (status, _, set_cookies, bytes) = net::read(
                net,
                Request::get(&url, headers),
                proxy,
                Duration::from_secs(20),
            )
            .await?;
            if status == 200 {
                if let Some(value) = data(&bytes) {
                    let mut jar = Jar::default();
                    for (key, value) in cached {
                        jar.seed(PLATFORM, key, value)?;
                    }
                    jar.take(&url, &set_cookies)?;
                    return Ok((jar.map(PLATFORM)?, value));
                }
            }
        }
        let (status, _, _, bytes) = net::read(
            net,
            Request::get(&url, BTreeMap::new()),
            proxy,
            Duration::from_secs(20),
        )
        .await?;
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if status != 401 {
            return Err(PluginError::transport(format!(
                "MiMo's platform returned HTTP {status}."
            )));
        }
        let login = account_url(core::required(&value, "loginUrl")?)?;
        let (reply, jar, at) = follow(net, creds, login.as_str(), PLATFORM, false, proxy).await?;
        let (status, _, _, bytes) = net::collect(reply, 16 * 1024 * 1024).await?;
        if status != 200
            || url::Url::parse(&at)
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
                .as_deref()
                != Some("platform.xiaomimimo.com")
        {
            return Err(PluginError::transport(
                "The Xiaomi account did not sign in at the MiMo platform.",
            ));
        }
        let value = data(&bytes).ok_or_else(|| {
            PluginError::transport("The Xiaomi account did not sign in at the MiMo platform.")
        })?;
        let cookies = jar.map(PLATFORM)?;
        if cookies.is_empty() {
            return Err(PluginError::transport(
                "MiMo's platform set no session cookies.",
            ));
        }
        Ok((cookies, value))
    })
    .await
    .map_err(|_| PluginError::transport("MiMo's platform sign-in timed out."))?
}
#[cfg(test)]
#[path = "../test/auth.test.rs"]
mod tests;
