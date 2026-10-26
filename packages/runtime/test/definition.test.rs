use super::*;
use serde_json::json;

fn package() -> Value {
    json!({"apiVersion":1,"id":"native-test","name":"Native Test","version":"1.0.0","api":"openai-completions","runtime":{"type":"native","entry":"provider"},"models":[],"config":{"keyProvider":{"private":"native schema"},"options":[{"id":"flag","type":"boolean"}]}})
}

#[test]
fn native_preparation_preserves_private_configuration_without_using_generic_schema() {
    let path = std::env::temp_dir().join(format!("nora-prepare-native-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    let source = path.join("source.json");
    let output = path.join("output");
    let original = package();
    std::fs::write(&source, serde_json::to_vec(&original).unwrap()).unwrap();
    assert!(Definition::parse(original.clone()).is_err());
    prepare(&source, &output).unwrap();
    let prepared: Value = serde_json::from_slice(
        &std::fs::read(output.join("native-test/nora-provider.json")).unwrap(),
    )
    .unwrap();
    let mut declared = original;
    crate::build_capabilities::declare(&mut declared);
    assert_eq!(prepared, declared);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn preparation_rejects_unsafe_identities_and_non_native_entrypoints() {
    for id in ["../outside", "a/b", "a\\b", "", "."] {
        let mut value = package();
        value["id"] = json!(id);
        assert!(validate_package(&value).is_err());
    }
    let mut value = package();
    value["runtime"]["type"] = json!("node");
    assert!(validate_package(&value).is_err());
    value = package();
    value["config"] = json!([]);
    assert!(validate_package(&value).is_err());
}

#[tokio::test]
async fn provider_owned_options_remain_readable_without_becoming_http_fields() {
    use base64::Engine;
    let definition = Definition::parse(json!({"id":"native-options","name":"Native Options","api":"openai-completions","baseURL":"https://api.example.invalid/v1","config":{"keyProvider":"openai","options":[{"id":"native_feature","type":"boolean","default":false}],"native_feature":true,"privateFeature":{"mode":"active"}}})).unwrap();
    assert_eq!(definition.config.extra["native_feature"], true);
    assert_eq!(
        definition.config.extra["privateFeature"],
        json!({"mode":"active"})
    );
    assert_eq!(
        definition.config.extra["options"][0]["id"],
        "native_feature"
    );
    assert!(!definition.config.extra.contains_key("keyProvider"));
    let body = br#"{ "model":"test", "messages":[] }"#;
    let request = crate::transport::build_request(
        &definition,
        nora_provider_plugin_sdk::FetchRequest {
            caller_user_agent: None,
            constraints: Default::default(),
            subscription_accounts_enabled: true,
            account: "fixture".into(),
            credentials: Some(json!({"type":"apiKey","access":"fixture-key"})),
            network_proxy: None,
            url: "https://api.example.invalid/v1/chat/completions".into(),
            method: "POST".into(),
            headers: BTreeMap::new(),
            metadata: BTreeMap::new(),
            body: Some(base64::engine::general_purpose::STANDARD.encode(body)),
            body_file: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(request.body.unwrap(), body);
    assert_eq!(
        request.url,
        "https://api.example.invalid/v1/chat/completions"
    );
    assert!(!request
        .headers
        .keys()
        .any(|name| name == "native_feature" || name == "privateFeature"));
}

#[test]
fn preparation_preserves_each_packages_explicit_connection_capability() {
    let path = std::env::temp_dir().join(format!(
        "nora-prepare-capabilities-{}",
        uuid::Uuid::new_v4()
    ));
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    prepare(source, &path).unwrap();
    let mut count = 0;
    for package in std::fs::read_dir(&path).unwrap() {
        let package = package.unwrap().path();
        let prepared: Value =
            serde_json::from_slice(&std::fs::read(package.join("nora-provider.json")).unwrap())
                .unwrap();
        let original: Value = serde_json::from_slice(
            &std::fs::read(
                source
                    .join(package.file_name().unwrap())
                    .join("nora-provider.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let mut declared = original;
        crate::build_capabilities::declare(&mut declared);
        assert_eq!(prepared, declared, "{}", prepared["id"]);
        if !source
            .join(package.file_name().unwrap())
            .join("Cargo.toml")
            .is_file()
        {
            assert_eq!(
                prepared["config"]["connectionOptions"], true,
                "Generic packages run through the shared runtime: {}",
                prepared["id"]
            );
        }
        count += 1;
    }
    assert!(count >= 91);
    for id in ["compatible", "openai", "anthropic", "xai", "google-gemini"] {
        let value: Value = serde_json::from_slice(
            &std::fs::read(source.join(id).join("nora-provider.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(value["config"]["connectionOptions"], true, "{id}");
    }
    for id in [
        "remote-magpie",
        "typesafe",
        "cloudflare-jev",
        "vercel-jev",
        "bailian-decision",
        "qwen-decision",
        "google-vertex",
        "amazon-bedrock",
        "kiro",
    ] {
        let value: Value = serde_json::from_slice(
            &std::fs::read(source.join(id).join("nora-provider.json")).unwrap(),
        )
        .unwrap();
        assert_ne!(
            value["config"]["connectionOptions"], true,
            "A partial implementation cannot advertise the complete connection feature: {id}"
        );
    }
    std::fs::remove_dir_all(path).unwrap();
}
#[test]
fn preparation_keeps_missing_null_false_and_true_capabilities_unchanged() {
    let path = std::env::temp_dir().join(format!(
        "nora-prepare-old-capabilities-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&path).unwrap();
    for (index, config) in [
        None,
        Some(Value::Null),
        Some(json!({"connectionOptions":false})),
        Some(json!({"connectionOptions":true})),
        Some(json!({"connectionOptions":null})),
    ]
    .into_iter()
    .enumerate()
    {
        let mut original = package();
        match config {
            Some(config) => original["config"] = config,
            None => {
                original.as_object_mut().unwrap().remove("config");
            }
        }
        let source = path.join(format!("source-{index}.json"));
        let output = path.join(format!("output-{index}"));
        std::fs::write(&source, serde_json::to_vec(&original).unwrap()).unwrap();
        prepare(&source, &output).unwrap();
        let result: Value = serde_json::from_slice(
            &std::fs::read(output.join("native-test/nora-provider.json")).unwrap(),
        )
        .unwrap();
        crate::build_capabilities::declare(&mut original);
        assert_eq!(result, original);
    }
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn cached_model_metadata_is_declared_only_by_packages_that_serve_the_common_rpc() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let path = entry.unwrap().path();
        let manifest = path.join("nora-provider.json");
        if !manifest.is_file() {
            continue;
        }
        let manifest: Value = serde_json::from_slice(&std::fs::read(manifest).unwrap()).unwrap();
        if !path.join("Cargo.toml").is_file() {
            assert_eq!(
                manifest["config"]["modelMetadata"], true,
                "Generic package {}",
                manifest["id"]
            );
        }
    }
    for id in [
        "compatible",
        "openai",
        "anthropic",
        "xai",
        "google-gemini",
        "github-copilot",
    ] {
        let value: Value = serde_json::from_slice(
            &std::fs::read(source.join(id).join("nora-provider.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(value["config"]["modelMetadata"], true, "{id}");
    }
    for id in [
        "remote-magpie",
        "kiro",
        "factory",
        "typesafe",
        "cloudflare-jev",
        "vercel-jev",
    ] {
        let value: Value = serde_json::from_slice(
            &std::fs::read(source.join(id).join("nora-provider.json")).unwrap(),
        )
        .unwrap();
        assert_ne!(
            value["config"]["modelMetadata"], true,
            "A direct provider needs its own explicit implementation: {id}"
        );
    }
}
