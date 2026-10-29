use super::*;
use std::path::PathBuf;

async fn provider() -> BundledProvider {
    provider_at(PathBuf::new()).await
}

async fn provider_at(directory: PathBuf) -> BundledProvider {
    let provider = BundledProvider::new(crate::openai_provider::OpenAI);
    provider
        .initialize(InitializeParams {
            subscription_accounts_enabled: true,
            installed_manifest: None,
            runtime_config: serde_json::Value::Null,
            api_version: API_VERSION,
            plugin_id: "openai".into(),
            plugin_directory: std::path::PathBuf::new(),
            data_directory: directory,
            manifest: serde_json::from_slice(
                &std::fs::read(
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .parent()
                        .unwrap()
                        .join("openai/nora-plugin.json"),
                )
                .unwrap(),
            )
            .unwrap(),
        })
        .await
        .unwrap();
    provider
}

#[tokio::test]
async fn native_provider_rejects_a_different_providers_manifest() {
    let provider = BundledProvider::new(crate::xai_provider::Xai::default());
    let result = provider
        .initialize(InitializeParams {
            subscription_accounts_enabled: true,
            installed_manifest: None,
            runtime_config: serde_json::Value::Null,
            api_version: API_VERSION,
            plugin_id: "openai".into(),
            plugin_directory: PathBuf::new(),
            data_directory: PathBuf::new(),
            manifest: serde_json::from_str(include_str!("../../openai/nora-plugin.json")).unwrap(),
        })
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn rejected_reinitialization_keeps_the_current_connection_and_proxy_together() {
    let provider = provider().await;
    let credentials = json!({"type":"apiKey","access":"fixture-key"});
    let original = provider
        .configuration()
        .unwrap()
        .account_definition(Some(&credentials))
        .unwrap();
    let mut invalid: Value =
        serde_json::from_str(include_str!("../../xai/nora-plugin.json")).unwrap();
    invalid["config"]["networkProxy"] =
        json!({"mode":"proxy","url":"http://replacement.invalid:8080"});
    let result = provider
        .initialize(InitializeParams {
            subscription_accounts_enabled: true,
            installed_manifest: None,
            runtime_config: json!({"baseURL":"https://replacement.invalid/v1"}),
            api_version: API_VERSION,
            plugin_id: "xai".into(),
            plugin_directory: PathBuf::new(),
            data_directory: std::env::temp_dir().join("rejected-provider-configuration"),
            manifest: invalid,
        })
        .await;
    assert!(result.is_err());
    assert_eq!(
        provider
            .configuration()
            .unwrap()
            .effective_proxy(None)
            .unwrap(),
        None
    );
    let retained = provider
        .configuration()
        .unwrap()
        .account_definition(Some(&credentials))
        .unwrap();
    assert_eq!(retained.id, original.id);
    assert_eq!(retained.base_url, original.base_url);
    assert_eq!(retained.api, original.api);
}

#[tokio::test]
async fn maintenance_finishes_against_a_valid_local_catalog_without_account_credentials() {
    let directory =
        std::env::temp_dir().join(format!("nora-provider-maintain-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("models.json"),serde_json::to_vec(&json!({"vendor":{"id":"vendor","models":{"fresh-model":{"id":"fresh-model","modalities":{"output":["text"]},"limit":{"context":100000,"output":10000}}}}})).unwrap()).unwrap();
    let provider = provider_at(directory.clone()).await;
    let result = provider
        .call(
            "maintain",
            json!({"credentials":{"access":"invalid-value-must-not-be-used"}}),
        )
        .await
        .unwrap();
    assert_eq!(result, json!({"ok":true,"modelsChanged":false}));
    assert!(provider.call("unknown", json!({})).await.is_err());
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn api_authorization_returns_a_readable_separate_account_value() {
    let provider = provider().await;
    let first = provider
        .authorize(json!({"method":"apiKey","inputs":{"apiKey":"first-private"}}))
        .await
        .unwrap();
    let second = provider
        .authorize(json!({"method":"apiKey","label":"Work","fields":{"key":"second-private"}}))
        .await
        .unwrap();
    assert_eq!(first["credentials"]["label"], "API key");
    assert_eq!(second["credentials"]["label"], "Work");
    assert_eq!(first["credentials"]["access"], "first-private");
    assert_eq!(second["credentials"]["access"], "second-private");
    assert_eq!(second["credentials"]["type"], "apiKey");
    assert!(provider
        .authorize(json!({"method":"apiKey","inputs":{"apiKey":"bad\nkey"}}))
        .await
        .is_err());
}

#[tokio::test]
async fn api_account_refresh_does_not_attempt_an_oauth_login() {
    let provider = provider().await;
    let value = provider
        .authorize(json!({"method":"apiKey","apiKey":"private-key"}))
        .await
        .unwrap();
    let refreshed = provider
        .refresh(json!({"credentials":value["credentials"],"force":true}))
        .await
        .unwrap();
    assert_eq!(refreshed["changed"], false);
}
#[tokio::test]
async fn saved_codex_accounts_derive_a_safe_profile_without_signing_in_again() {
    use base64::Engine;
    let provider = provider().await;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"workspace-id","chatgpt_plan_type":"plus"},"https://api.openai.com/profile":{"email":"historic@example.com","name":"Historic Name"}}"#);
    let credentials = json!({"type":"oauth","credentialProvider":"openAICodex","access":format!("header.{payload}.signature"),"refresh":"private-refresh"});
    let accounts = provider
        .accounts(CallContext {
            subscription_accounts_enabled: true,
            account: "work".into(),
            network_proxy: None,
            credentials: Some(credentials),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        accounts["accounts"][0],
        json!({"id":"work","email":"historic@example.com","name":"Historic Name","plan":"plus"})
    );
    let visible = accounts["accounts"].to_string();
    assert!(!visible.contains("private-refresh"));
    assert!(!visible.contains("signature"));
    assert!(!visible.contains("workspace-id"));
}

#[tokio::test]
async fn api_account_usage_exposes_its_profile_without_requiring_a_usage_endpoint() {
    let provider = provider().await;
    let usage = provider.usage(CallContext { subscription_accounts_enabled: true,
        account:"personal".into(),
        network_proxy:None,
        credentials:Some(json!({"type":"apiKey","access":"private-api-secret","extras":{"email":"api@example.com","name":"API Member","plan":"team"}})),
        ..Default::default()
    }).await.unwrap();
    assert_eq!(usage["account"]["email"], "api@example.com");
    assert_eq!(usage["account"]["id"], "personal");
    assert_eq!(usage["account"]["plan"], "team");
    assert!(!usage.to_string().contains("private-api-secret"));
}
