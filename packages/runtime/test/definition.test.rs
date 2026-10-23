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
    assert_eq!(prepared, original);
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
