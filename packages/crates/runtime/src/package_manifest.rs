use nora_plugin_manifest::PluginManifest;
use serde_json::Value;
use std::path::{Component, Path};

/// A package's `nora-plugin.json`, checked by plugin-kit, with the checks
/// only a published provider package needs: a version usable in archive
/// names and local sign-ins Nora can find before installing it.
pub(crate) fn parse(value: &Value) -> Result<PluginManifest, String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    let manifest = PluginManifest::parse(&bytes).map_err(|error| error.to_string())?;
    if !filename_component(&manifest.version) {
        return Err("The provider package version is invalid.".into());
    }
    if let Some(provider) = &manifest.contributes.provider {
        validate_local_sign_ins(&provider.local_sign_ins, &provider.auth_methods)?;
    }
    Ok(manifest)
}

fn validate_local_sign_ins(local_sign_ins: &[Value], auth_methods: &[Value]) -> Result<(), String> {
    if local_sign_ins.len() > 8 {
        return Err(
            "The provider package localSignIns must be an array of at most 8 entries.".into(),
        );
    }
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
                return Err("A local sign-in file must be a plain absolute or home path.".into());
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
    Ok(())
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
