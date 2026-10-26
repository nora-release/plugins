use super::*;

fn manifest(id: &str) -> Value {
    serde_json::from_slice(
        &std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join(id)
                .join("nora-provider.json"),
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn required_login_prompts_are_declared_where_authorization_requires_an_answer() {
    for (id, method, key, kind) in [
        ("autoclaw", "oauth", "vendor", "select"),
        ("autoclaw-cn", "oauth", "phone", "text"),
        ("qoder", "pat", "pat", "password"),
        ("qoder-cn", "pat", "pat", "password"),
        ("qoder-cn", "vpc", "vpc", "text"),
        ("qwenwork-enterprise", "oauth", "domain", "text"),
    ] {
        let manifest = manifest(id);
        let method = manifest["authMethods"]
            .as_array()
            .unwrap()
            .iter()
            .find(|value| value["id"] == method)
            .unwrap();
        let prompt = method["prompts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|value| value["key"] == key)
            .unwrap();
        assert_eq!(prompt["type"], kind, "{id}/{key}");
        assert_eq!(prompt["required"], true, "{id}/{key}");
        if kind == "select" {
            assert_eq!(
                prompt["options"],
                json!([{"label":"Google","value":"google"},{"label":"z.ai","value":"zai"}])
            );
        }
    }
    let optional = manifest("trae-cn");
    let method = optional["authMethods"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value["id"] == "oauth-client")
        .unwrap();
    assert_ne!(method["prompts"][0]["required"], true);
}

#[test]
fn preparing_native_packages_preserves_prompt_schema_and_does_not_add_defaults() {
    let root = std::env::temp_dir().join(format!(
        "nora-auth-prompt-contract-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    for id in [
        "autoclaw",
        "autoclaw-cn",
        "qoder",
        "qoder-cn",
        "qwenwork-enterprise",
        "trae-cn",
    ] {
        let original = manifest(id);
        let source = root.join(format!("{id}.json"));
        std::fs::write(&source, serde_json::to_vec(&original).unwrap()).unwrap();
        prepare(&source, &root.join("output")).unwrap();
        let prepared: Value = serde_json::from_slice(
            &std::fs::read(root.join("output").join(id).join("nora-provider.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(prepared["authMethods"], original["authMethods"], "{id}");
    }
    std::fs::remove_dir_all(root).unwrap();
}
