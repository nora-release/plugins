use super::*;
use nora_provider_plugin_sdk::{CallContext, InitializeParams, ProviderPlugin};
use serde_json::{json, Value};
use std::path::PathBuf;

fn manifest() -> Value {
    serde_json::from_str(include_str!("../nora-provider.json")).unwrap()
}
fn params(manifest: Value) -> InitializeParams {
    InitializeParams {
        api_version: 1,
        plugin_id: "qwen-decision".into(),
        plugin_directory: PathBuf::new(),
        data_directory: PathBuf::new(),
        manifest,
    }
}
#[test]
fn manifest_declares_decision_only_and_explicit_authentication() {
    let value = manifest();
    assert_eq!(value["id"], DecisionFamily::Qwen.id());
    assert_eq!(value["api"], "systemone");
    assert_eq!(value["config"]["operations"], json!(["decision"]));
    let allowed = value["config"]["operationsAuthMethods"]["decision"]
        .as_array()
        .unwrap();
    for method in value["authMethods"].as_array().unwrap() {
        assert!(allowed.contains(&method["id"]));
    }
    for model in value["models"].as_array().unwrap() {
        assert_eq!(model["info"]["decision"], true);
        assert_eq!(model["info"]["chat"], false);
        assert_eq!(model["info"]["catalogSource"], "preset");
    }
}
#[tokio::test]
async fn unconfigured_catalog_stays_a_preset_without_network_or_claimed_authentication() {
    let plugin = DecisionProvider::new(DecisionFamily::Qwen);
    plugin.initialize(params(manifest())).await.unwrap();
    let models = plugin.models(CallContext::default()).await.unwrap();
    assert_eq!(models["source"], "preset");
    assert!(!models["models"].as_array().unwrap().is_empty());
    assert!(plugin
        .authorize(json!({"authMethodID":"oauth"}))
        .await
        .is_err());
    assert!(plugin
        .refresh(json!({"credentials":{"type":"oauth","access":"foreign"}}))
        .await
        .is_err());
}
#[tokio::test]
async fn another_provider_manifest_cannot_initialize_this_executable() {
    let plugin = DecisionProvider::new(DecisionFamily::Qwen);
    let mut value = manifest();
    value["id"] = json!("another-provider");
    assert!(plugin.initialize(params(value)).await.is_err());
}
