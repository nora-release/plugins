use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn each_standalone_plugin_uses_the_shared_macos_tls_configuration() {
    let output = std::process::Command::new(env!("CARGO"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "metadata",
            "--locked",
            "--offline",
            "--no-deps",
            "--format-version",
            "1",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout).unwrap();
    let packages: BTreeMap<&str, &Value> = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["name"].as_str().unwrap(), p))
        .collect();
    for name in packages.keys() {
        assert!(
            system_tls(name, &packages, true, &mut BTreeSet::new()),
            "{name} must inherit system TLS when built on its own for macOS"
        );
        assert!(
            !system_tls(name, &packages, false, &mut BTreeSet::new()),
            "{name} must retain its existing TLS backend on other systems"
        );
    }
}

fn system_tls<'a>(
    name: &'a str,
    packages: &BTreeMap<&'a str, &'a Value>,
    macos: bool,
    visited: &mut BTreeSet<&'a str>,
) -> bool {
    if !visited.insert(name) {
        return false;
    }
    let Some(package) = packages.get(name) else {
        return false;
    };
    package["dependencies"]
        .as_array()
        .unwrap()
        .iter()
        .any(|dep| {
            if !dep["kind"].is_null() {
                return false;
            }
            if dep["target"].as_str() == Some("cfg(target_os = \"macos\")") && !macos {
                return false;
            }
            let name = dep["name"].as_str().unwrap();
            if name == "rustls-platform-verifier" {
                return true;
            }
            system_tls(name, packages, macos, visited)
        })
}

#[test]
fn plugin_sources_cannot_bypass_the_shared_network_entrypoint() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    for package in std::fs::read_dir(root).unwrap() {
        let package = package.unwrap().path();
        if package.file_name().unwrap() == "network" {
            continue;
        }
        check_sources(&package.join("src"));
    }
}
fn check_sources(path: &std::path::Path) {
    if !path.exists() {
        return;
    }
    for file in std::fs::read_dir(path).unwrap() {
        let file = file.unwrap().path();
        if file.is_dir() {
            check_sources(&file);
            continue;
        }
        if file.extension().and_then(|v| v.to_str()) != Some("rs") {
            continue;
        }
        let source = std::fs::read_to_string(&file).unwrap();
        for bypass in [
            "nora_provider_plugin_sdk::http_client",
            "reqwest::Client::builder",
            "reqwest::Client::new",
            "ReqwestHttpClient::new()",
        ] {
            assert!(
                !source.contains(bypass),
                "{} bypasses platform trust using {bypass}",
                file.display()
            );
        }
    }
}
