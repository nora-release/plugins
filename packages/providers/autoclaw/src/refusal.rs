use super::*;
use regex::Regex;
use std::sync::LazyLock;

pub(super) const CODE: &str = "account_refused";
static BANNED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bbanned\b|封禁|封号|账号异常|账户异常|account (has been |is |was )?(suspended|frozen|disabled|blocked|restricted)|账[号户](已|已被|被)?(冻结|停用|禁用)")
        .expect("Static AutoClaw account refusal pattern")
});
static REVOKED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)invalid[_ ]?grant|revoked|expired|登录已失效|token 失效")
        .expect("Static AutoClaw token refusal pattern")
});

pub(super) fn code(value: &Value) -> &Value {
    value
        .get("code")
        .filter(|value| !value.is_null())
        .or_else(|| value.pointer("/error/code"))
        .unwrap_or(&Value::Null)
}

pub(super) fn message(value: &Value) -> &str {
    value
        .get("message")
        .filter(|value| !value.is_null())
        .or_else(|| value.get("msg").filter(|value| !value.is_null()))
        .or_else(|| value.pointer("/error/message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
}

pub(super) fn banned(value: &Value) -> Option<String> {
    let message = message(value);
    if number(code(value)) == Some(410004.0) {
        return Some(if message.is_empty() {
            "account refused (code 410004)".into()
        } else {
            message.into()
        });
    }
    (!message.is_empty() && BANNED.is_match(message)).then(|| message.into())
}

pub(super) fn revoked(message: &str) -> bool {
    REVOKED.is_match(message)
}

pub(super) fn redact(message: &str, secrets: &[&str]) -> String {
    let mut message = message.to_owned();
    for secret in secrets
        .iter()
        .map(|value| auth::bare(value))
        .filter(|value| !value.is_empty())
    {
        message = message.replace(secret, "[redacted]");
    }
    message.chars().take(2048).collect()
}
