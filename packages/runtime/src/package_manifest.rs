use serde_json::Value;
use std::path::{Component, Path};

/// The package envelope shared by native plugins. Its config belongs to the
/// executable and is deliberately independent of the generic HTTP adapter.
pub(crate) struct PackageManifest<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub version: &'a str,
    pub api_version: u64,
    pub minimum_host_version: Option<&'a str>,
    pub auth_methods: &'a [Value],
    pub config: Option<&'a serde_json::Map<String, Value>>,
    pub local_sign_ins: &'a [Value],
}

impl<'a> PackageManifest<'a> {
    pub fn parse(value: &'a Value) -> Result<Self, String> {
        if !value.is_object() {
            return Err("The provider package manifest must be an object.".into());
        }
        let id = required_text(value, "id", 128)?;
        if !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err("The provider package identity is invalid.".into());
        }
        let name = required_text(value, "name", 128)?;
        let version = required_text(value, "version", 64)?;
        if !filename_component(version) {
            return Err("The provider package version is invalid.".into());
        }
        required_text(value, "api", 128)?;
        let api_version = value
            .get("apiVersion")
            .and_then(Value::as_u64)
            .filter(|version| *version == 1)
            .ok_or("The provider package requires API version 1.")?;
        let runtime = value
            .get("runtime")
            .filter(|runtime| runtime.is_object())
            .ok_or("The provider package requires a native executable.")?;
        if runtime.get("type").and_then(Value::as_str) != Some("native") {
            return Err("The provider package requires a native executable.".into());
        }
        let entry = required_text(runtime, "entry", 1024)?;
        let entry_path = Path::new(entry);
        if entry_path.is_absolute()
            || entry.contains('\\')
            || entry_path
                .components()
                .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
            || entry_path
                .components()
                .all(|part| matches!(part, Component::CurDir))
            || entry_path
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|extension| {
                    matches!(
                        extension,
                        "js" | "mjs" | "cjs" | "ts" | "mts" | "py" | "sh" | "bash" | "rb" | "pl"
                    )
                })
        {
            return Err(
                "The provider executable must be a relative compiled entry inside its package."
                    .into(),
            );
        }
        if value.get("baseURL").is_some_and(|value| !value.is_string()) {
            return Err("The provider package baseURL must be a string.".into());
        }
        array(value, "models", 4096)?;
        let auth_methods = array(value, "authMethods", 32)?;
        if auth_methods.iter().any(|method| !method.is_object()) {
            return Err("The provider sign-in methods must be objects.".into());
        }
        let config = match value.get("config") {
            None | Some(Value::Null) => None,
            Some(Value::Object(config)) => Some(config),
            _ => return Err("The provider package configuration must be an object.".into()),
        };
        let aliases = array(value, "aliases", 4096)?;
        if aliases.iter().any(|alias| !alias.is_string()) {
            return Err("The provider aliases must be strings.".into());
        }
        let minimum_host_version = value
            .get("minimumHostVersion")
            .filter(|value| !value.is_null())
            .map(|_| required_text(value, "minimumHostVersion", 64))
            .transpose()?;
        let local_sign_ins = array(value, "localSignIns", 8)?;
        for sign_in in local_sign_ins {
            let method = required_text(sign_in, "method", 64)?;
            required_text(sign_in, "label", 128)?;
            if !auth_methods
                .iter()
                .any(|auth| auth.get("id").and_then(Value::as_str) == Some(method))
            {
                return Err("A local sign-in names an unknown sign-in method.".into());
            }
            let paths = array(sign_in, "paths", 8)?;
            let keychain = sign_in
                .get("keychain")
                .filter(|value| !value.is_null())
                .map(|_| required_text(sign_in, "keychain", 128))
                .transpose()?;
            if paths.is_empty() && keychain.is_none() {
                return Err("A local sign-in requires a file or Keychain item.".into());
            }
            for path in paths {
                let path = path
                    .as_str()
                    .filter(|path| valid_text(path, 1024))
                    .ok_or("A local sign-in file is invalid.")?;
                let plain = path.strip_prefix("~/").unwrap_or(path);
                if !(path.starts_with("~/") || Path::new(path).is_absolute())
                    || Path::new(plain)
                        .components()
                        .any(|part| part == Component::ParentDir)
                {
                    return Err(
                        "A local sign-in file must be a plain absolute or home path.".into(),
                    );
                }
            }
            if let Some(inputs) = sign_in.get("inputs") {
                let inputs = inputs
                    .as_object()
                    .filter(|inputs| inputs.len() <= 8)
                    .ok_or("A local sign-in declares invalid inputs.")?;
                if inputs.iter().any(|(key, value)| {
                    !valid_text(key, 64) || value.as_str().is_none_or(|value| value.len() > 1024)
                }) {
                    return Err("A local sign-in declares invalid inputs.".into());
                }
            }
        }
        Ok(Self {
            id,
            name,
            version,
            api_version,
            minimum_host_version,
            auth_methods,
            config,
            local_sign_ins,
        })
    }
}

fn valid_text(text: &str, limit: usize) -> bool {
    !text.trim().is_empty() && text.len() <= limit && !text.chars().any(char::is_control)
}

fn required_text<'a>(value: &'a Value, field: &str, limit: usize) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| valid_text(text, limit))
        .ok_or_else(|| format!("The provider package {field} is invalid."))
}

fn array<'a>(value: &'a Value, field: &str, limit: usize) -> Result<&'a [Value], String> {
    match value.get(field) {
        None => Ok(&[]),
        Some(Value::Array(items)) if items.len() <= limit => Ok(items),
        _ => Err(format!(
            "The provider package {field} must be an array of at most {limit} entries."
        )),
    }
}

pub(crate) fn filename_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && !matches!(value, "." | "..")
}
