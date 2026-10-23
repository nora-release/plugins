use nora_ai::{HttpClient, HttpMethod};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use nora_provider_subscription_core as core;
use serde_json::Value;
use std::collections::BTreeMap;
pub(crate) const ZCODE: &str = "https://zcode.z.ai";
pub(crate) const VERSION: &str = "3.14.3";
pub(crate) const START: &str = "https://zcode.z.ai/api/v1/zcode-plan/anthropic";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Site {
    Zai,
    BigModel,
}
impl Site {
    pub(crate) fn parse(value: &str) -> PluginResult<Self> {
        match value {
            "zai" => Ok(Self::Zai),
            "bigmodel" => Ok(Self::BigModel),
            _ => Err(PluginError::invalid("Select Z.ai or BigModel explicitly.")),
        }
    }
    pub(crate) fn id(self) -> &'static str {
        if self == Self::Zai {
            "zai"
        } else {
            "bigmodel"
        }
    }
    pub(crate) fn api(self) -> &'static str {
        if self == Self::Zai {
            "https://api.z.ai"
        } else {
            "https://bigmodel.cn"
        }
    }
    pub(crate) fn base(self) -> &'static str {
        if self == Self::Zai {
            "https://api.z.ai/api/anthropic"
        } else {
            "https://open.bigmodel.cn/api/anthropic"
        }
    }
    pub(crate) fn root(self) -> &'static str {
        if self == Self::Zai {
            "https://api.z.ai"
        } else {
            "https://open.bigmodel.cn"
        }
    }
}
pub(crate) fn platform() -> String {
    format!(
        "{}-{}",
        if cfg!(target_os = "macos") {
            "darwin"
        } else if cfg!(target_os = "windows") {
            "win32"
        } else {
            "linux"
        },
        if cfg!(target_arch = "aarch64") {
            "arm64"
        } else if cfg!(target_arch = "x86_64") {
            "x64"
        } else {
            std::env::consts::ARCH
        }
    )
}
#[derive(Debug)]
pub(crate) struct Error {
    pub status: u16,
    pub message: String,
}
impl From<PluginError> for Error {
    fn from(error: PluginError) -> Self {
        Self {
            status: 0,
            message: error.message,
        }
    }
}
impl From<Error> for PluginError {
    fn from(error: Error) -> Self {
        if error.status == 401 {
            PluginError::authentication(error.message)
        } else {
            PluginError::transport(error.message)
        }
    }
}
pub(crate) async fn call(
    client: &dyn HttpClient,
    method: HttpMethod,
    url: &str,
    auth: Option<&str>,
    body: Option<&Value>,
    headers: BTreeMap<String, String>,
    device: &str,
) -> Result<Value, Error> {
    let mut request = core::json_request(method, url, body, None)?;
    request
        .headers
        .insert("user-agent".into(), format!("ZCode/{VERSION}"));
    if let Some(auth) = auth.filter(|s| !s.is_empty()) {
        if auth.len() > 128 * 1024 || auth.contains(['\r', '\n', '\0']) {
            return Err(PluginError::invalid("Invalid ZCode authorization value.").into());
        }
        request.headers.insert("authorization".into(), auth.into());
    }
    if url.starts_with(&format!("{ZCODE}/")) {
        request.headers.insert("x-device-mid".into(), device.into());
    }
    request.headers.extend(headers);
    let (status, value) = core::exchange(client, request).await?;
    if !(200..300).contains(&status) {
        return Err(Error {
            status,
            message: format!("ZCode returned HTTP {status}; the account was kept."),
        });
    }
    if !value.is_object() {
        return Err(Error {
            status,
            message: "ZCode returned invalid JSON.".into(),
        });
    }
    let code = value
        .get("code")
        .filter(|value| !value.is_null())
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string())
        })
        .unwrap_or_default();
    if !code.is_empty() && code != "0" && code != "200" {
        return Err(Error {
            status,
            message: format!(
                "ZCode refused the operation (code {}).",
                code.chars().take(32).collect::<String>()
            ),
        });
    }
    Ok(value.get("data").cloned().unwrap_or(Value::Null))
}
pub(crate) fn team_headers(site: Site, org: &str, project: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("bigmodel-organization".into(), org.into()),
        ("bigmodel-project".into(), project.into()),
        (
            "set-language".into(),
            if site == Site::BigModel { "zh" } else { "en" }.into(),
        ),
        ("accept-language".into(), "en-US,en".into()),
    ])
}
pub(crate) fn expiry(jwt: &str) -> u64 {
    core::decode_jwt(jwt)["exp"]
        .as_u64()
        .unwrap_or(0)
        .saturating_mul(1000)
}
pub(crate) fn live_jwt(jwt: &str) -> PluginResult<()> {
    if jwt.is_empty() || expiry(jwt) > 0 && expiry(jwt) <= core::now() {
        return Err(PluginError::authentication(
            "ZCode's Start Plan sign-in expired. Sign in to ZCode again.",
        ));
    }
    Ok(())
}
