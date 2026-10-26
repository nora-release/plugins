use super::*;
use std::path::PathBuf;

fn native_manifest() -> Value {
    json!({"apiVersion":1,"id":"example","name":"Example","version":"1.0.0","runtime":{"type":"native","entry":"provider"},"api":"openai-completions","baseURL":"https://example.test/v1","models":[],"authMethods":[],"config":{}})
}

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "nora-catalog-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("definitions/example")).unwrap();
        std::fs::create_dir(root.join("archives")).unwrap();
        std::fs::write(
            root.join("definitions/example/nora-provider.json"),
            native_manifest().to_string(),
        )
        .unwrap();
        Self(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn catalog_can_list_source_packages_without_claiming_a_download_exists() {
    let fixture = Fixture::new();
    let output = fixture.0.join("providers.json");
    write_catalog(&fixture.0.join("definitions"), &output, None).unwrap();
    let value: Value = serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
    assert_eq!(value["providers"][0]["id"], "example");
    assert_eq!(value["providers"][0]["packages"], json!({}));
    assert!(value["providers"][0].get("localSignIns").is_none());
}

#[test]
fn catalog_carries_the_local_sign_ins_a_provider_declares() {
    let fixture = Fixture::new();
    let sign_ins =
        json!([{"method":"desktop","label":"Example app","paths":["~/.example/auth.json"]}]);
    let mut manifest = native_manifest();
    manifest["authMethods"] = json!([{"id":"desktop","type":"oauth","label":"Example app"}]);
    manifest["localSignIns"] = sign_ins.clone();
    std::fs::write(
        fixture.0.join("definitions/example/nora-provider.json"),
        manifest.to_string(),
    )
    .unwrap();
    let output = fixture.0.join("providers.json");
    write_catalog(&fixture.0.join("definitions"), &output, None).unwrap();
    let value: Value = serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
    assert_eq!(value["providers"][0]["localSignIns"], sign_ins);
}

#[test]
fn release_catalog_hashes_the_actual_archives_and_uses_the_selected_platform() {
    let fixture = Fixture::new();
    let output = fixture.0.join("providers.json");
    let bytes = b"archive contents";
    std::fs::write(
        fixture
            .0
            .join("archives/example-1.0.0-aarch64-apple-darwin.zip"),
        bytes,
    )
    .unwrap();
    write_catalog(
        &fixture.0.join("definitions"),
        &output,
        Some((
            &fixture.0.join("archives"),
            "aarch64-apple-darwin",
            "https://example.test/releases/v1",
        )),
    )
    .unwrap();
    let value: Value = serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
    assert_eq!(
        value["providers"][0]["packages"]["aarch64-apple-darwin"]["sha256"],
        format!("{:x}", Sha256::digest(bytes))
    );
    assert_eq!(
        value["providers"][0]["packages"]["aarch64-apple-darwin"]["url"],
        "https://example.test/releases/v1/example-1.0.0-aarch64-apple-darwin.zip"
    );
}

#[test]
fn catalog_generation_refuses_missing_archives() {
    let fixture = Fixture::new();
    let output = fixture.0.join("providers.json");
    assert!(write_catalog(
        &fixture.0.join("definitions"),
        &output,
        Some((
            &fixture.0.join("archives"),
            "aarch64-apple-darwin",
            "https://example.test/releases/v1"
        ))
    )
    .is_err());
    assert!(!output.exists());
}

#[test]
fn native_private_configuration_is_opaque_to_preparation_and_catalog_generation() {
    let fixture = Fixture::new();
    let mut manifest = native_manifest();
    manifest["minimumHostVersion"] = json!("0.2.0");
    manifest["authMethods"] = json!([{"id":"browser","type":"oauth","label":"Sign in"}]);
    manifest["config"] = json!({"keyProvider":{"nativeOnly":true},"description":"Native account","privateOptions":{"credential":"must-not-be-published"}});
    assert!(crate::definition::Definition::parse(manifest.clone()).is_err());
    let source = fixture.0.join("definitions/example/nora-provider.json");
    std::fs::write(&source, manifest.to_string()).unwrap();
    let output = fixture.0.join("providers.json");
    write_catalog(&fixture.0.join("definitions"), &output, None).unwrap();
    let bytes = std::fs::read(&output).unwrap();
    let catalog: Value = serde_json::from_slice(&bytes).unwrap();
    let provider = &catalog["providers"][0];
    assert_eq!(provider["description"], "Native account");
    assert_eq!(provider["apiVersion"], 1);
    assert_eq!(provider["minimumHostVersion"], "0.2.0");
    assert!(!String::from_utf8(bytes)
        .unwrap()
        .contains("must-not-be-published"));
    assert!(provider.get("config").is_none());
    let prepared = fixture.0.join("prepared");
    crate::definition::prepare(&source, &prepared).unwrap();
    let package: Value = serde_json::from_slice(
        &std::fs::read(prepared.join("example/nora-provider.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        package["config"]["keyProvider"],
        manifest["config"]["keyProvider"]
    );
    assert_eq!(
        package["config"]["privateOptions"],
        manifest["config"]["privateOptions"]
    );
}

fn all_packages() -> (PathBuf, BTreeMap<String, Value>) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let mut manifests = BTreeMap::new();
    for entry in std::fs::read_dir(&root).unwrap() {
        let path = entry.unwrap().path().join("nora-provider.json");
        if !path.is_file() {
            continue;
        }
        let value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let id = PackageManifest::parse(&value)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
            .id
            .to_owned();
        assert!(manifests.insert(id, value).is_none());
    }
    assert!(
        manifests.len() >= 106,
        "The regression must cover the complete existing provider collection."
    );
    (root, manifests)
}

#[test]
fn source_catalog_includes_every_native_and_generic_package_without_adapter_fields() {
    let fixture = Fixture::new();
    let (root, manifests) = all_packages();
    let output = fixture.0.join("providers.json");
    write_catalog(&root, &output, None).unwrap();
    let catalog: Value = serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
    let providers = catalog["providers"].as_array().unwrap();
    assert_eq!(providers.len(), manifests.len());
    let actual = providers
        .iter()
        .map(|provider| provider["id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(actual, manifests.keys().cloned().collect::<Vec<_>>());
    for provider in providers {
        let manifest = &manifests[provider["id"].as_str().unwrap()];
        assert_eq!(provider["name"], manifest["name"]);
        assert_eq!(provider["version"], manifest["version"]);
        assert_eq!(provider["apiVersion"], manifest["apiVersion"]);
        assert_eq!(provider["packages"], json!({}));
        if manifest["localSignIns"]
            .as_array()
            .is_some_and(|sign_ins| !sign_ins.is_empty())
        {
            assert_eq!(provider["localSignIns"], manifest["localSignIns"]);
        }
    }
    for native in [
        "joycode",
        "qwenwork",
        "qwenwork-enterprise",
        "factory",
        "cline",
        "brave-search",
        "tavily",
    ] {
        assert!(
            manifests[native]["config"].get("keyProvider").is_none(),
            "{native} must exercise a genuinely independent native schema"
        );
        assert!(actual.iter().any(|id| id == native));
    }
}

#[test]
fn release_catalog_covers_all_packages_and_hashes_each_exact_archive() {
    let fixture = Fixture::new();
    let (root, manifests) = all_packages();
    let output = fixture.0.join("providers.json");
    let archives = fixture.0.join("archives");
    let target = "aarch64-apple-darwin";
    for (id, manifest) in &manifests {
        let version = manifest["version"].as_str().unwrap();
        let body = format!("archive bytes for {id}@{version}");
        std::fs::write(archives.join(format!("{id}-{version}-{target}.zip")), body).unwrap();
    }
    write_catalog(
        &root,
        &output,
        Some((
            &archives,
            target,
            "https://example.test/releases/providers-v1.2.0/",
        )),
    )
    .unwrap();
    let catalog: Value = serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
    let providers = catalog["providers"].as_array().unwrap();
    assert_eq!(providers.len(), manifests.len());
    for provider in providers {
        let id = provider["id"].as_str().unwrap();
        let version = provider["version"].as_str().unwrap();
        let filename = format!("{id}-{version}-{target}.zip");
        let package = &provider["packages"][target];
        assert_eq!(provider["packages"].as_object().unwrap().len(), 1);
        assert_eq!(
            package["url"],
            format!("https://example.test/releases/providers-v1.2.0/{filename}")
        );
        assert_eq!(
            package["sha256"],
            format!(
                "{:x}",
                Sha256::digest(std::fs::read(archives.join(filename)).unwrap())
            )
        );
    }
}

#[test]
fn invalid_package_envelopes_are_rejected_by_preparation_and_catalog_together() {
    let fixture = Fixture::new();
    let source = fixture.0.join("definitions/example/nora-provider.json");
    let output = fixture.0.join("providers.json");
    let mutations = [
        ("/apiVersion", json!(2)),
        ("/apiVersion", json!("1")),
        ("/id", json!("../escape")),
        ("/id", json!("")),
        ("/name", json!(" ")),
        ("/version", json!("../1.0")),
        ("/api", json!(false)),
        ("/runtime/type", json!("node")),
        ("/runtime/entry", json!("../outside")),
        ("/runtime/entry", json!("/absolute")),
        ("/runtime/entry", json!("./")),
        ("/runtime/entry", json!("index.mjs")),
        ("/models", json!({})),
        ("/models", json!(vec![Value::Null; 4097])),
        ("/authMethods", json!({})),
        ("/authMethods", json!(["invalid"])),
        ("/config", json!([])),
        ("/baseURL", json!(32)),
    ];
    for (pointer, value) in mutations {
        let mut manifest = native_manifest();
        *manifest.pointer_mut(pointer).unwrap() = value;
        std::fs::write(&source, manifest.to_string()).unwrap();
        std::fs::write(&output, b"previous-catalog").unwrap();
        assert!(
            crate::definition::validate_package(&manifest).is_err(),
            "{pointer}: {manifest}"
        );
        assert!(
            crate::definition::prepare(&source, &fixture.0.join("prepared")).is_err(),
            "{pointer}"
        );
        let error = write_catalog(&fixture.0.join("definitions"), &output, None).unwrap_err();
        assert!(error.contains("nora-provider.json"), "{error}");
        assert_eq!(std::fs::read(&output).unwrap(), b"previous-catalog");
    }
}

#[test]
fn invalid_local_sign_in_metadata_is_never_published() {
    let fixture = Fixture::new();
    let source = fixture.0.join("definitions/example/nora-provider.json");
    let output = fixture.0.join("providers.json");
    for sign_ins in [
        json!({}),
        json!([{"method":"unknown","label":"App","paths":["~/.auth"]}]),
        json!([{"method":"desktop","label":"App","paths":["~/../escape"]}]),
        json!([{"method":"desktop","label":"App","paths":["relative/auth"]}]),
        json!([{"method":"desktop","label":"App","inputs":{"path":false}}]),
    ] {
        let mut manifest = native_manifest();
        manifest["authMethods"] = json!([{"id":"desktop","type":"oauth"}]);
        manifest["localSignIns"] = sign_ins;
        std::fs::write(&source, manifest.to_string()).unwrap();
        assert!(write_catalog(&fixture.0.join("definitions"), &output, None).is_err());
        assert!(!output.exists());
    }
}

#[test]
fn malformed_json_duplicates_and_empty_sources_leave_the_previous_catalog_untouched() {
    let fixture = Fixture::new();
    let source = fixture.0.join("definitions/example/nora-provider.json");
    let output = fixture.0.join("providers.json");
    std::fs::write(&output, b"previous-catalog").unwrap();
    std::fs::write(&source, b"{broken").unwrap();
    assert!(write_catalog(&fixture.0.join("definitions"), &output, None).is_err());
    assert_eq!(std::fs::read(&output).unwrap(), b"previous-catalog");
    std::fs::write(&source, native_manifest().to_string()).unwrap();
    let duplicate = fixture.0.join("definitions/duplicate");
    std::fs::create_dir_all(&duplicate).unwrap();
    std::fs::copy(&source, duplicate.join("nora-provider.json")).unwrap();
    assert!(write_catalog(&fixture.0.join("definitions"), &output, None)
        .unwrap_err()
        .contains("duplicate"));
    assert_eq!(std::fs::read(&output).unwrap(), b"previous-catalog");
    let empty = fixture.0.join("empty");
    std::fs::create_dir(&empty).unwrap();
    assert!(write_catalog(&empty, &output, None).is_err());
    assert_eq!(std::fs::read(output).unwrap(), b"previous-catalog");
}

#[test]
fn release_catalog_rejects_unsafe_platforms_and_non_plain_https_download_bases() {
    let fixture = Fixture::new();
    let output = fixture.0.join("providers.json");
    let archives = fixture.0.join("archives");
    std::fs::write(
        archives.join("example-1.0.0-aarch64-apple-darwin.zip"),
        b"archive",
    )
    .unwrap();
    for target in ["../escape", "/absolute", ".", "..", ""] {
        assert!(write_catalog(
            &fixture.0.join("definitions"),
            &output,
            Some((&archives, target, "https://example.test/releases"))
        )
        .is_err());
        assert!(!output.exists());
    }
    for base in [
        "http://example.test/releases",
        "https://token@example.test/releases",
        "https://user:pass@example.test/releases",
        "https://example.test/releases?token=secret",
        "https://example.test/releases#fragment",
        "not-a-url",
    ] {
        assert!(
            write_catalog(
                &fixture.0.join("definitions"),
                &output,
                Some((&archives, "aarch64-apple-darwin", base))
            )
            .is_err(),
            "{base}"
        );
        assert!(!output.exists());
    }
}
