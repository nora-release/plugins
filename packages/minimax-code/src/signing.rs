use md5::{Digest, Md5};
use nora_ai::{HttpMethod, HttpRequest};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use nora_provider_subscription_core as core;
use serde_json::Value;
#[derive(Clone, Copy)]
pub(crate) struct Site {
    pub id: &'static str,
    pub account: &'static str,
    pub llm: &'static str,
    pub agent: &'static str,
    pub platform: &'static str,
    pub region: &'static str,
    pub lang: &'static str,
}
impl Site {
    pub fn for_id(id: &str) -> PluginResult<Self> {
        match id {
            "minimax-code" => Ok(Self {
                id: "minimax-code",
                account: "https://account.minimax.cn",
                llm: "https://agent.minimax.cn",
                agent: "https://agent.minimaxi.com",
                platform: "https://www.minimaxi.com",
                region: "cn",
                lang: "zh",
            }),
            "minimax-code-global" => Ok(Self {
                id: "minimax-code-global",
                account: "https://account.minimax.io",
                llm: "https://agent.minimax.io",
                agent: "https://agent.minimax.io",
                platform: "https://platform.minimax.io",
                region: "en",
                lang: "en",
            }),
            _ => Err(PluginError::invalid(
                "This executable requires a MiniMax Code manifest.",
            )),
        }
    }
    pub fn base(&self) -> String {
        format!("{}/mavis/api/v1/llm/v1", self.llm)
    }
}
pub(crate) fn md5(value: &str) -> String {
    hex::encode(Md5::digest(value.as_bytes()))
}
pub(crate) fn signed(
    site: Site,
    path: &str,
    access: &str,
    user: &str,
    body: Option<&Value>,
    origin: &str,
    now: u64,
    tz: i32,
    platform: &str,
) -> PluginResult<HttpRequest> {
    let mut url = url::Url::parse(&format!("{origin}{path}"))
        .map_err(|_| PluginError::invalid("The MiniMax account address is invalid."))?;
    let now_string = now.to_string();
    let timezone = tz.to_string();
    let os = platform;
    url.query_pairs_mut().extend_pairs([
        ("device_platform", "mcode"),
        ("biz_id", "3"),
        ("app_id", "3001"),
        ("version_code", "22201"),
        ("unix", now_string.as_str()),
        ("timezone_offset", timezone.as_str()),
        ("sys_language", site.lang),
        ("lang", site.lang),
        ("device_id", "0"),
        ("os_name", os),
        ("browser_name", "mcode"),
        ("user_id", if user.is_empty() { "0" } else { user }),
        ("client", "mcode"),
    ]);
    let suffix = format!("{}?{}", url.path(), url.query().unwrap_or(""));
    let bytes = body
        .map(serde_json::to_string)
        .transpose()
        .map_err(|_| PluginError::invalid("The MiniMax account request cannot be encoded."))?;
    let ts = (now / 1000).to_string();
    let yy = md5(&format!(
        "{}_{}{}ooui",
        core::encode(&suffix),
        bytes.as_deref().unwrap_or("{}"),
        md5(&now_string)
    ));
    let signature = md5(&format!(
        "{ts}I*7Cf%WZ#S&%1RlZJ&C2{}",
        bytes.as_deref().unwrap_or("")
    ));
    let mut request = core::json_request(
        if body.is_some() {
            HttpMethod::Post
        } else {
            HttpMethod::Get
        },
        url.as_str(),
        None,
        Some(access),
    )?
    .with_header("content-type", "application/json")
    .with_header("user-agent", "MiniMaxCode")
    .with_header("yy", yy)
    .with_header("x-timestamp", ts)
    .with_header("x-signature", signature);
    request.body = bytes.map(String::into_bytes);
    Ok(request)
}
#[cfg(test)]
#[path = "../test/signing.test.rs"]
mod tests;
