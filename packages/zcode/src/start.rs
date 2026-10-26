use crate::endpoint::{self, Site};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::BTreeMap;
fn prompt() -> Value {
    serde_json::from_str(include_str!("prompt.json")).unwrap()
}
fn os_version() -> String {
    #[cfg(unix)]
    {
        let mut data = std::mem::MaybeUninit::<libc::utsname>::uninit();
        if unsafe { libc::uname(data.as_mut_ptr()) } == 0 {
            let data = unsafe { data.assume_init() };
            let release =
                unsafe { std::ffi::CStr::from_ptr(data.release.as_ptr()) }.to_string_lossy();
            return format!(
                "{} {release} {}",
                if cfg!(target_os = "macos") {
                    "darwin"
                } else {
                    "linux"
                },
                if cfg!(target_arch = "aarch64") {
                    "arm64"
                } else {
                    std::env::consts::ARCH
                }
            );
        }
    }
    format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
}
pub(crate) fn headers(jwt: &str) -> PluginResult<BTreeMap<String, String>> {
    endpoint::live_jwt(jwt)?;
    let mut headers = BTreeMap::from([
        ("authorization".into(), format!("Bearer {jwt}")),
        ("anthropic-version".into(), "2023-06-01".into()),
        ("http-referer".into(), endpoint::ZCODE.into()),
        (
            "user-agent".into(),
            format!("ZCode/{} ai-sdk/anthropic/3.0.81", endpoint::VERSION),
        ),
        ("x-zcode-app-version".into(), endpoint::VERSION.into()),
        ("x-title".into(), "Z Code@cli".into()),
        ("x-release-channel".into(), "production".into()),
        (
            "x-client-language".into(),
            std::env::var("LANG")
                .ok()
                .and_then(|value| {
                    value
                        .split('.')
                        .next()
                        .filter(|value| {
                            !value.is_empty() && value.len() < 64 && !value.contains(['\r', '\n'])
                        })
                        .map(|value| value.replace('_', "-"))
                })
                .unwrap_or_else(|| "unknown".into()),
        ),
        ("x-zcode-agent".into(), "glm".into()),
        ("x-platform".into(), endpoint::platform()),
        (
            "x-os-category".into(),
            if cfg!(target_os = "macos") {
                "macos"
            } else if cfg!(target_os = "windows") {
                "windows"
            } else {
                "linux"
            }
            .into(),
        ),
        ("x-os-version".into(), os_version()),
        ("x-request-id".into(), uuid::Uuid::new_v4().to_string()),
        ("x-zcode-session-type".into(), "main".into()),
        ("x-zcode-trace-id".into(), uuid::Uuid::new_v4().to_string()),
        ("content-type".into(), "application/json".into()),
    ]);
    if let Ok(zone) = iana_time_zone::get_timezone() {
        headers.insert("x-client-timezone".into(), zone);
    }
    Ok(headers)
}
pub(crate) fn dress(
    mut body: Value,
    site: Site,
    device: &str,
    directory: Option<&str>,
    git: Option<bool>,
) -> PluginResult<Value> {
    let prompt = prompt();
    if !body["messages"].is_array() {
        return Err(PluginError::invalid(
            "ZCode Start requires an Anthropic Messages body.",
        ));
    }
    let environment = &prompt["environment"];
    let mut lines = vec![
        environment["heading"].as_str().unwrap().to_owned(),
        environment["invokedLine"].as_str().unwrap().to_owned(),
    ];
    if let Some(directory) = directory {
        lines.push(format!(
            "- {}: {directory}",
            environment["cwdLabel"].as_str().unwrap()
        ));
        if let Some(git) = git {
            lines.push(format!(
                "- {}: {}",
                environment["gitLabel"].as_str().unwrap(),
                if git { "yes" } else { "no" }
            ));
        }
    }
    lines.push(format!(
        "- {}: {}",
        environment["platformLabel"].as_str().unwrap(),
        if cfg!(target_os = "macos") {
            "darwin"
        } else if cfg!(target_os = "windows") {
            "win32"
        } else {
            "linux"
        }
    ));
    lines.push(format!(
        "- {}: {}",
        environment["osVersionLabel"].as_str().unwrap(),
        os_version()
    ));
    if let Some(model) = body["model"].as_str() {
        lines.push(
            environment["poweredByLine"]
                .as_str()
                .unwrap()
                .replace(
                    "{provider}",
                    if site == Site::Zai {
                        "zai-api"
                    } else {
                        "bigmodel-api"
                    },
                )
                .replace("{model}", model),
        );
    }
    let cached =
        |text: String| json!({"type":"text","text":text,"cache_control":{"type":"ephemeral"}});
    let stable = prompt["stableSections"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut system = vec![
        cached(prompt["cliPrefix"].as_str().unwrap().into()),
        cached(stable),
        cached(format!(
            "\n\n{}\n\n{}\n\n{}",
            prompt["beforeEnvironment"].as_str().unwrap(),
            lines.join("\n"),
            prompt["afterEnvironment"].as_str().unwrap()
        )),
    ];
    if let Some(text) = body["system"]
        .as_str()
        .filter(|text| !text.trim().is_empty())
    {
        system.push(json!({"type":"text","text":text}));
    } else if let Some(blocks) = body["system"].as_array() {
        for block in blocks {
            if block["type"] != "text" || !block["text"].is_string() {
                return Err(PluginError::invalid(
                    "ZCode Start supports text-only system content.",
                ));
            }
            if !block["text"].as_str().unwrap().is_empty() {
                system.push(json!({"type":"text","text":block["text"]}));
            }
        }
    }
    body["system"] = json!(system);
    let context = &prompt["context"];
    let content = format!(
        "<system-reminder>{}\n{}\n\n{}</system-reminder>",
        context["intro"].as_str().unwrap(),
        format!(
            "{}\n{}",
            context["currentDateHeading"].as_str().unwrap(),
            context["currentDateLine"].as_str().unwrap().replace(
                "{date}",
                &chrono::Local::now().format("%Y-%m-%d").to_string()
            )
        ),
        context["outro"].as_str().unwrap()
    );
    let messages = body["messages"].as_array_mut().unwrap();
    messages.insert(
        0,
        json!({"role":"user","content":[{"type":"text","text":content}]}),
    );
    let mut last = None;
    for (index, message) in messages.iter_mut().enumerate() {
        if message["role"] == "system" {
            continue;
        }
        last = Some(index);
        for block in message["content"].as_array_mut().into_iter().flatten() {
            if let Some(object) = block.as_object_mut() {
                object.remove("cache_control");
            }
        }
    }
    if let Some(last) = last {
        let content = &mut messages[last]["content"];
        if let Some(text) = content.as_str() {
            *content = json!([cached(text.to_owned())]);
        } else if let Some(block) = content
            .as_array_mut()
            .and_then(|blocks| blocks.last_mut())
            .filter(|block| block.is_object())
        {
            block["cache_control"] = json!({"type":"ephemeral"});
        }
    }
    for tool in body["tools"].as_array_mut().into_iter().flatten() {
        if let Some(tool) = tool.as_object_mut() {
            tool.remove("cache_control");
        }
    }
    if !body["metadata"].is_object() {
        body["metadata"] = json!({});
    }
    body["metadata"]["user_id"] = json!(crate::device::user_id(device));
    Ok(body)
}
pub(crate) fn spent(value: &Value, status: u16) -> bool {
    status == 429
        || [&value["code"], &value["error"]["code"]]
            .iter()
            .any(|code| {
                matches!(code.as_str(), Some("1113" | "1005"))
                    || matches!(code.as_u64(), Some(1113 | 1005))
            })
}
pub(crate) fn refused(status: u16, bytes: &[u8]) -> bool {
    if status < 400 {
        return false;
    }
    let text = String::from_utf8_lossy(bytes).to_lowercase();
    status == 405
        || text.contains("request has been blocked")
        || text.contains("errors.aliyun.com")
        || text.contains("unusual activity")
        || serde_json::from_slice::<Value>(bytes)
            .ok()
            .is_some_and(|value| value["code"] == 3012 || value["code"] == "3012")
}
pub(crate) fn blocked(status: u16) -> Value {
    json!({"type":"error","error":{"type":"permission_error","message":format!("ZCode Start returned HTTP {status} despite using its native request format. This can be an IP block or a changed vendor check. Use an account with a GLM Coding Plan or another provider.")}})
}
#[cfg(test)]
#[path = "../test/start.test.rs"]
mod tests;
