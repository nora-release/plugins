use crate::package_manifest::filename_component;
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
            .join(nora_plugin_manifest::package::MANIFEST_FILE);
        if !path.is_file() {
            continue;
        }
        let value: Value =
            serde_json::from_slice(&std::fs::read(&path).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        let manifest = crate::package_manifest::parse(&value)
            .map_err(|error| format!("Invalid provider manifest {}: {error}", path.display()))?;
        let version = manifest.version.as_str();
        let provider = manifest.contributes.provider.as_ref();
        let auth_methods = provider
            .map(|provider| provider.auth_methods.as_slice())
            .unwrap_or_default();
        let description = manifest.summary.as_deref().unwrap_or_else(|| {
            if auth_methods
                .iter()
                .any(|method| method.get("type").and_then(Value::as_str) == Some("oauth"))
            {
                "Connect your subscription or API key."
            } else {
                "Connect with your API key."
            }
        });
        if description.len() > 4096 || description.chars().any(char::is_control) {
            return Err(format!(
                "The provider description is invalid: {}",
                manifest.id
            ));
        }
        let mut packages = BTreeMap::new();
        if let Some((archives, target, base)) = release {
            if !filename_component(target) {
                return Err("The release target is invalid".into());
            }
            let filename = format!("{}-{version}-{target}.zip", manifest.id);
            let path = archives.join(&filename);
            let mut file = std::fs::File::open(&path)
                .map_err(|error| format!("Missing provider archive {}: {error}", manifest.id))?;
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
            if base.scheme() != "https"
                || base.host_str().is_none()
                || !base.username().is_empty()
                || base.password().is_some()
                || base.query().is_some()
                || base.fragment().is_some()
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
        let mut entry = json!({"apiVersion":2,"id":manifest.id,"name":manifest.name,"description":description,"version":version,"repository":format!("https://github.com/nora-release/plugins/tree/agent/router/packages/{}",manifest.id),"contributes":contributions(&manifest),"packages":packages});
        if let Some(minimum) = &manifest.minimum_host_version {
            entry["minimumHostVersion"] = json!(minimum);
        }
        // Nora finds these sign-ins on a Mac before the provider is installed.
        if let Some(sign_ins) = provider
            .map(|provider| &provider.local_sign_ins)
            .filter(|sign_ins| !sign_ins.is_empty())
        {
            entry["localSignIns"] = json!(sign_ins);
        }
        if providers.insert(manifest.id.clone(), entry).is_some() {
            return Err("The provider catalog contains duplicate IDs".into());
        }
    }
    if providers.is_empty() {
        return Err("No providers were found".into());
    }
    let catalog =
        json!({"schemaVersion":2,"providers":providers.into_values().collect::<Vec<_>>()});
    std::fs::write(
        output,
        serde_json::to_vec_pretty(&catalog).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

/// What the package adds, named as Nora's catalog lists it.
fn contributions(manifest: &nora_plugin_manifest::PluginManifest) -> Vec<&'static str> {
    let contributes = &manifest.contributes;
    [
        (contributes.provider.is_some(), "provider"),
        (contributes.hooks.is_some(), "hooks"),
        (contributes.middleware.is_some(), "middleware"),
        (contributes.masker.is_some(), "masker"),
        (!contributes.panels.is_empty(), "panels"),
        (contributes.page.is_some(), "page"),
        (!contributes.actions.is_empty(), "actions"),
    ]
    .into_iter()
    .filter_map(|(present, name)| present.then_some(name))
    .collect()
}

#[cfg(test)]
#[path = "../test/catalog.test.rs"]
mod tests;
