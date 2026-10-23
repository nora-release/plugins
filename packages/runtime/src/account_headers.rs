use crate::{definition::Definition, discovery};
use nora_ai::{JsonValue, OAuthCredentials};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(crate) fn validate(definition: &Definition) -> PluginResult<()> {
    let mut seen = std::collections::BTreeSet::new();
    if definition.config.header_hints.len() > 32
        || definition
            .config
            .header_hints
            .iter()
            .any(|name| !valid_name(name) || !seen.insert(name.to_ascii_lowercase()))
    {
        return Err(invalid());
    }
    Ok(())
}

pub(crate) fn methods(definition: &Definition) -> Vec<Value> {
    let mut methods = definition.auth_methods.clone();
    let relay = discovery::configured_bases(definition, None)
        .iter()
        .any(|base| crate::auth_headers::relay(definition, &base.api, &base.base_url));
    for method in &mut methods {
        if !matches!(
            method.get("type").and_then(Value::as_str),
            Some("api" | "apiKey")
        ) {
            continue;
        }
        if method.get("fields").is_none() {
            method["fields"] = json!([]);
        }
        let Some(fields) = method.get_mut("fields").and_then(Value::as_array_mut) else {
            continue;
        };
        if fields.is_empty() {
            fields.push(json!({"id":"apiKey","key":"apiKey","type":"password","label":"API key","required":!definition.config.key_optional}));
        }
        for name in definition
            .config
            .header_hints
            .iter()
            .filter(|name| valid_name(name))
        {
            let key = format!("header:{name}");
            if !fields.iter().any(|field| {
                field
                    .get("key")
                    .or_else(|| field.get("id"))
                    .and_then(Value::as_str)
                    == Some(key.as_str())
            }) {
                fields
                    .push(json!({"id":key,"key":key,"type":"text","label":name,"required":false}));
            }
        }
        if relay
            && !fields.iter().any(|field| {
                field
                    .get("key")
                    .or_else(|| field.get("id"))
                    .and_then(Value::as_str)
                    == Some("authHeader")
            })
        {
            fields.push(json!({"id":"authHeader","key":"authHeader","type":"text","label":"Key header (auto, x-api-key, or bearer)","required":false,"placeholder":"auto"}));
        }
    }
    methods
}

pub(crate) fn assign(
    definition: &Definition,
    inputs: &Value,
    credentials: &mut OAuthCredentials,
) -> PluginResult<()> {
    if let Some(value) = inputs.get("authHeader") {
        let value = value.as_str().ok_or_else(invalid)?;
        if !valid_value(value) {
            return Err(invalid());
        }
        let value = value.trim();
        if !matches!(value, "" | "auto" | "bearer" | "x-api-key") {
            return Err(invalid());
        }
        if !matches!(value, "" | "auto") {
            credentials
                .extras
                .insert("authHeader".into(), JsonValue::string(value));
        }
    }
    let mut headers = BTreeMap::new();
    for name in &definition.config.header_hints {
        if !valid_name(name) {
            return Err(invalid());
        }
        let Some(value) = inputs
            .get(format!("header:{name}"))
            .or_else(|| inputs.get("headers").and_then(|headers| headers.get(name)))
        else {
            continue;
        };
        let value = value.as_str().ok_or_else(invalid)?;
        if !valid_value(value) {
            return Err(invalid());
        }
        let value = value.trim();
        if !value.is_empty() {
            headers.insert(name.clone(), value.to_string());
        }
    }
    if !headers.is_empty() {
        credentials
            .extras
            .insert("noraAccountHeaders".into(), JsonValue::from(json!(headers)));
    }
    Ok(())
}

pub(crate) fn read(value: &Value) -> PluginResult<BTreeMap<String, String>> {
    let Some(values) = value
        .pointer("/extras/noraAccountHeaders")
        .or_else(|| value.get("noraAccountHeaders"))
    else {
        return Ok(BTreeMap::new());
    };
    let values = values
        .as_object()
        .filter(|values| values.len() <= 32)
        .ok_or_else(invalid)?;
    values
        .iter()
        .map(|(name, value)| {
            let value = value.as_str().ok_or_else(invalid)?;
            if !valid_name(name) || !valid_value(value) {
                return Err(invalid());
            }
            Ok((name.clone(), value.into()))
        })
        .collect()
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
        && !matches!(
            name.to_ascii_lowercase().as_str(),
            "host"
                | "content-length"
                | "connection"
                | "transfer-encoding"
                | "authorization"
                | "x-api-key"
                | "api-key"
                | "proxy-authorization"
                | "cookie"
                | "set-cookie"
        )
}
fn valid_value(value: &str) -> bool {
    value.len() <= 8192 && !value.chars().any(char::is_control)
}
fn invalid() -> PluginError {
    PluginError::invalid("The account's optional request headers are invalid.")
}

#[cfg(test)]
#[path = "../test/account_headers.test.rs"]
mod tests;
