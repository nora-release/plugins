use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

pub fn write_catalog(
    definitions: &Path,
    output: &Path,
    release: Option<(&Path, &str, &str)>,
) -> Result<(), String> {
    let mut providers = BTreeMap::new();
    for entry in std::fs::read_dir(definitions).map_err(|error| error.to_string())? {
        let path = entry
            .map_err(|error| error.to_string())?
            .path()
            .join("nora-provider.json");
        if !path.is_file() {
            continue;
        }
        let value: Value =
            serde_json::from_slice(&std::fs::read(&path).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        let definition = crate::definition::Definition::parse(value.clone())
            .map_err(|error| error.to_string())?;
        let version = value
            .get("version")
            .and_then(Value::as_str)
            .ok_or("A provider version is required")?;
        if !safe_component(&definition.id) || !safe_component(version) {
            return Err("A provider ID or version contains invalid filename characters".into());
        }
        let description = value
            .pointer("/config/description")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                if definition
                    .auth_methods
                    .iter()
                    .any(|method| method.get("type").and_then(Value::as_str) == Some("oauth"))
                {
                    "Connect your subscription or API key."
                } else {
                    "Connect with your API key."
                }
            });
        let mut packages = BTreeMap::new();
        if let Some((archives, target, base)) = release {
            if !safe_component(target) {
                return Err("The release target is invalid".into());
            }
            let filename = format!("{}-{version}-{target}.zip", definition.id);
            let path = archives.join(&filename);
            let mut file = std::fs::File::open(&path)
                .map_err(|error| format!("Missing provider archive {}: {error}", definition.id))?;
            let mut digest = Sha256::new();
            let mut bytes = [0; 64 * 1024];
            loop {
                let size = file.read(&mut bytes).map_err(|error| error.to_string())?;
                if size == 0 {
                    break;
                }
                digest.update(&bytes[..size]);
            }
            let base = url::Url::parse(&format!("{}/", base.trim_end_matches('/')))
                .map_err(|_| "The package download base URL is invalid")?;
            if base.scheme() != "https" || !base.username().is_empty() || base.password().is_some()
            {
                return Err(
                    "Published provider packages require an HTTPS download base URL".into(),
                );
            }
            let url = base
                .join(&filename)
                .map_err(|_| "The package download URL is invalid")?;
            packages.insert(
                target,
                json!({"url":url.as_str(),"sha256":format!("{:x}",digest.finalize())}),
            );
        }
        let provider = json!({
            "id":definition.id,"name":definition.name,"description":description,"version":version,
            "repository":format!("https://github.com/nora-release/plugins/tree/agent/router/packages/{}",definition.id),
            "packages":packages
        });
        if providers.insert(definition.id, provider).is_some() {
            return Err("The provider catalog contains duplicate IDs".into());
        }
    }
    if providers.is_empty() {
        return Err("No providers were found".into());
    }
    let catalog =
        json!({"schemaVersion":1,"providers":providers.into_values().collect::<Vec<_>>()});
    std::fs::write(
        output,
        serde_json::to_vec_pretty(&catalog).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && !matches!(value, "." | "..")
}

#[cfg(test)]
#[path = "../test/catalog.test.rs"]
mod tests;
