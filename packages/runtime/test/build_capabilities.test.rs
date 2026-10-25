use super::*;

#[test]
fn preparation_declares_exact_current_versions_without_assuming_legacy_actions_are_automatic() {
    for config in [
        Value::Null,
        json!({}),
        json!({"accountActions":["checkin","consumeReset"]}),
        json!({"accountAutomation":{"version":2}}),
    ] {
        let mut manifest = json!({"config":config});
        declare(&mut manifest);
        assert_eq!(
            manifest["config"]["sdkCapabilities"],
            json!({"subscriptionPolicy":1,"automaticProxy":1})
        );
    }
    let mut manifest = json!({"config":{"connectionOptions":true,"accountImport":{"formats":["codex-auth"]},"accountAutomation":{"version":1},"sdkCapabilities":{"accountAutomation":99,"oldUnsupported":1}}});
    declare(&mut manifest);
    assert_eq!(
        manifest["config"]["sdkCapabilities"],
        json!({"subscriptionPolicy":1,"automaticProxy":1,"connectionOverrides":1,"accountImport":1,"accountAutomation":1})
    );
    let original = manifest.clone();
    declare(&mut manifest);
    assert_eq!(manifest, original);
}

#[test]
fn dynamic_model_resolution_is_declared_only_when_the_package_names_its_dynamic_models() {
    for models in [Value::Null, json!([]), json!(["auto"])] {
        let mut manifest = json!({"config":{"modelResolutionModels":models}});
        declare(&mut manifest);
        assert_eq!(
            manifest["config"]["sdkCapabilities"]["modelResolution"] == 1,
            models == json!(["auto"])
        );
    }
}

#[tokio::test]
async fn the_common_binary_only_claims_its_own_implemented_features() {
    use nora_provider_plugin_sdk::{InitializeParams, ProviderPlugin, API_VERSION};
    let manifest = json!({"id":"fixture","name":"Fixture","api":"openai-completions","baseURL":"https://fixture.invalid/v1","config":{"keyProvider":"fixture","accountImport":{"formats":["invented"]},"accountAutomation":{"version":1},"sdkCapabilities":{"accountImport":1,"accountAutomation":1}}});
    let provider = crate::BundledProvider::default();
    let initialized = provider
        .initialize(InitializeParams {
            subscription_accounts_enabled: true,
            installed_manifest: None,
            runtime_config: Value::Null,
            api_version: API_VERSION,
            plugin_id: "fixture".into(),
            plugin_directory: Default::default(),
            data_directory: Default::default(),
            manifest,
        })
        .await
        .unwrap();
    let features = &initialized["capabilities"]["sdkFeatures"];
    assert_eq!(features["connectionOverrides"], 1);
    assert!(features.get("accountImport").is_none());
    assert!(features.get("accountAutomation").is_none());
}
