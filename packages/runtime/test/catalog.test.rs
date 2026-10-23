use super::*;
use std::path::PathBuf;

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
        std::fs::write(root.join("definitions/example/nora-provider.json"), json!({
            "id":"example","name":"Example","version":"1.0.0","api":"openai-completions","baseURL":"https://example.test/v1",
            "config":{"keyProvider":"example"}
        }).to_string()).unwrap();
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
    std::fs::write(fixture.0.join("definitions/example/nora-provider.json"), json!({
        "id":"example","name":"Example","version":"1.0.0","api":"openai-completions","baseURL":"https://example.test/v1",
        "config":{"keyProvider":"example"},"localSignIns":sign_ins
    }).to_string()).unwrap();
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
