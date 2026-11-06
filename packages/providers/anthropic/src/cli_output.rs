use super::cli::{self, Home};
use nora_provider_plugin_sdk::PluginError;
use serde_json::Value;
use std::process::Output;

/// Claude's own configuration can enable verbose JSON despite --setting-sources.
pub(crate) fn result(bytes: &[u8]) -> Option<Value> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    match value {
        Value::Object(_) => Some(value),
        Value::Array(mut messages) => messages
            .iter()
            .rposition(|message| message["type"] == "result")
            .map(|index| messages.swap_remove(index)),
        _ => None,
    }
}

fn sign_in_required(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "oauth session expired and could not be refreshed",
        "oauth token revoked",
        "oauth access token has been revoked",
        "not logged in",
        "not signed in",
        "signed out",
        "sign-in has expired",
        "sign-in expired",
        "invalid access token",
    ]
    .iter()
    .any(|text| lower.contains(text))
}

pub(crate) async fn failure(output: &Output, home: &Home) -> PluginError {
    let payload = result(&output.stdout);
    if payload.as_ref().is_some_and(|payload| {
        payload["api_error_status"].as_u64() == Some(401)
            || payload["type"] == "authentication_error"
            || payload.pointer("/error/type").and_then(Value::as_str)
                == Some("authentication_error")
    }) {
        return cli::error();
    }
    let errors = payload
        .as_ref()
        .and_then(|value| value["errors"].as_array())
        .map(|errors| {
            errors
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let result = payload.as_ref().and_then(|value| {
        value["result"]
            .as_str()
            .or_else(|| value.pointer("/error/message").and_then(Value::as_str))
    });
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let message = [result.unwrap_or_default(), &errors, &stderr, &stdout]
        .into_iter()
        .map(str::trim)
        .find(|text| !text.is_empty());
    let Some(message) = message else {
        return PluginError::transport(format!(
            "Claude Code stopped without a result ({}).",
            output.status
        ));
    };
    if sign_in_required(message) {
        return cli::error();
    }

    let secrets = cli::credentials(home)
        .await
        .ok()
        .map(|credentials| {
            ["accessToken", "refreshToken", "idToken"]
                .into_iter()
                .filter_map(|key| credentials[key].as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let message = nora_provider::account_import::redact(message.to_owned(), &secrets);
    // A failed refresh can print the new token before Nora reads it back.
    let tokens = regex::Regex::new(
        r"(?i)sk-ant-(?:oat|ort|api)\d+-[A-Za-z0-9_-]+|Bearer\s+[A-Za-z0-9._~+/-]+=*",
    )
    .unwrap();
    let message = tokens.replace_all(&message, "[redacted]");
    let ansi = regex::Regex::new(r"\x1b\[[0-9;]*[A-Za-z]").unwrap();
    let message = ansi.replace_all(&message, "");
    let message = message
        .chars()
        .filter(|character| !character.is_control() || *character == '\n')
        .take(300)
        .collect::<String>();
    PluginError::transport(format!("Claude Code: {message}"))
}
