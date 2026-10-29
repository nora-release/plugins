use super::*;
use nora_ai::OAuthProviderId;

fn definition() -> Definition {
    Definition::from_fields(json!({"id":"relay-headers","name":"Relay","api":"anthropic-messages","baseURL":"https://relay.invalid/v1","authMethods":[{"id":"apiKey","type":"api","fields":[{"id":"apiKey","key":"apiKey","type":"password"}]},{"id":"oauth","type":"oauth"}],"config":{"keyProvider":"relay","headerHints":["anthropic-workspace-id","HTTP-Referer"]}})).unwrap()
}

#[test]
fn optional_header_fields_are_declared_for_api_accounts_without_changing_oauth() {
    let methods = methods(&definition());
    let fields = methods[0]["fields"].as_array().unwrap();
    assert!(
        fields
            .iter()
            .any(|field| field["key"] == "header:anthropic-workspace-id"
                && field["required"] == false)
    );
    assert!(fields.iter().any(|field| field["key"] == "authHeader"));
    assert!(methods[1].get("fields").is_none());
}

#[test]
fn custom_headers_and_explicit_key_choice_are_saved_only_in_the_current_account() {
    let definition = definition();
    let mut one = OAuthCredentials::api_key(OAuthProviderId::ApiKey, "first");
    assign(&definition, &json!({"header:anthropic-workspace-id":"workspace-one","header:HTTP-Referer":" https://app.invalid ","authHeader":"bearer"}), &mut one).unwrap();
    let mut two = OAuthCredentials::api_key(OAuthProviderId::ApiKey, "second");
    assign(
        &definition,
        &json!({"header:anthropic-workspace-id":"workspace-two"}),
        &mut two,
    )
    .unwrap();
    let one = serde_json::to_value(one).unwrap();
    let two = serde_json::to_value(two).unwrap();
    assert_eq!(
        read(&one).unwrap()["anthropic-workspace-id"],
        "workspace-one"
    );
    assert_eq!(
        read(&two).unwrap()["anthropic-workspace-id"],
        "workspace-two"
    );
    assert_eq!(read(&one).unwrap()["HTTP-Referer"], "https://app.invalid");
    assert_eq!(one["extras"]["authHeader"], "bearer");
    assert!(definition.config.headers.is_empty());
}

#[test]
fn header_values_types_and_auth_choices_are_validated_without_sending_a_request() {
    for inputs in [
        json!({"header:HTTP-Referer":"value\r\nInjected: yes"}),
        json!({"header:HTTP-Referer":123}),
        json!({"authHeader":"try-everything"}),
    ] {
        let mut credentials = OAuthCredentials::api_key(OAuthProviderId::ApiKey, "token");
        assert!(assign(&definition(), &inputs, &mut credentials).is_err());
    }
    for name in ["Host", "Authorization", "Cookie", "bad\nheader"] {
        assert!(read(&json!({"extras":{"noraAccountHeaders":{name:"value"}}})).is_err());
    }
}

#[tokio::test]
async fn account_headers_are_applied_privately_after_auth_and_never_added_to_model_info() {
    let definition = definition();
    let credentials = json!({"type":"apiKey","access":"key","extras":{"noraAccountHeaders":{"anthropic-workspace-id":"workspace-one"},"authHeader":"bearer"}});
    let request = nora_provider_plugin_sdk::FetchRequest {
        caller_user_agent: None,
        constraints: Default::default(),
        subscription_accounts_enabled: true,
        account: "one".into(),
        credentials: Some(credentials.clone()),
        network_proxy: None,
        url: "https://relay.invalid/v1/messages".into(),
        method: "POST".into(),
        headers: BTreeMap::new(),
        metadata: BTreeMap::new(),
        body: None,
        body_file: None,
    };
    let sent = crate::transport::build_request(&definition, request)
        .await
        .unwrap();
    assert_eq!(sent.headers["anthropic-workspace-id"], "workspace-one");
    assert_eq!(sent.headers["Authorization"], "Bearer key");
    assert!(!sent.headers.contains_key("x-api-key"));
    let mut model = nora_ai::Model::new("actual-model", "anthropic-messages", "relay");
    crate::models::configure_model(&definition, Some(&credentials), &mut model);
    let encoded = serde_json::to_value(model).unwrap().to_string();
    assert!(!encoded.contains("workspace-one"));
    assert!(!encoded.contains("authHeader"));
}

#[tokio::test]
async fn declared_native_authentication_fields_persist_headers_in_only_the_authorized_account() {
    use nora_provider_plugin_sdk::{CallContext, InitializeParams, ProviderPlugin};
    let plugin = crate::BundledProvider::new(crate::GenericProvider);
    let manifest = json!({"id":"header-lifecycle","name":"Headers","contributes":{"provider":{"api":"anthropic-messages","baseURL":"https://relay.invalid/v1","authMethods":[{"id":"apiKey","type":"api","fields":[{"id":"apiKey","key":"apiKey","type":"password"}]}],"config":{"keyProvider":"headers","headerHints":["anthropic-workspace-id"]}}}});
    plugin
        .initialize(InitializeParams {
            subscription_accounts_enabled: true,
            installed_manifest: None,
            runtime_config: serde_json::Value::Null,
            api_version: 2,
            plugin_id: "header-lifecycle".into(),
            plugin_directory: "/tmp/header-lifecycle".into(),
            data_directory: "/tmp/header-lifecycle".into(),
            manifest,
        })
        .await
        .unwrap();
    let methods = plugin.auth_methods(CallContext::default()).await.unwrap();
    assert!(methods[0]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .any(|field| field["key"] == "header:anthropic-workspace-id"));
    let connected = plugin.authorize(json!({"authMethodID":"apiKey","inputs":{"apiKey":"private-key","header:anthropic-workspace-id":"workspace-one","authHeader":"x-api-key"}})).await.unwrap();
    assert_eq!(
        connected["credentials"]["extras"]["noraAccountHeaders"]["anthropic-workspace-id"],
        "workspace-one"
    );
    assert_eq!(
        connected["credentials"]["extras"]["authHeader"],
        "x-api-key"
    );
    let other = plugin
        .authorize(json!({"authMethodID":"apiKey","inputs":{"apiKey":"other-key"}}))
        .await
        .unwrap();
    assert!(other["credentials"]["extras"]
        .get("noraAccountHeaders")
        .is_none());
}
