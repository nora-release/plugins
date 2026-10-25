use nora_provider_plugin_sdk::{InitializeParams, ProviderPlugin};
use serde_json::{json, Value};

fn manifest() -> Value {
    serde_json::from_str(include_str!("../../compatible/nora-provider.json")).unwrap()
}

#[tokio::test]
async fn an_explicit_private_auth_header_does_not_add_other_authentication_headers() {
    for scheme in [
        nora_ai::AuthScheme::ApiKeyHeader {
            name: "x-private-key".into(),
        },
        nora_ai::AuthScheme::None,
    ] {
        let mut value = manifest();
        value["baseURL"] = json!("https://relay.invalid");
        value["api"] = json!("anthropic-messages");
        value["config"]["apiKeyAuth"] = serde_json::to_value(&scheme).unwrap();
        let definition = crate::definition::Definition::parse(value).unwrap();
        let request = nora_provider_plugin_sdk::FetchRequest {
            subscription_accounts_enabled: true,
            account: "account".into(),
            credentials: Some(json!({"type":"apiKey","access":"private-custom-key"})),
            network_proxy: None,
            url: "https://relay.invalid/v1/messages".into(),
            method: "POST".into(),
            headers: Default::default(),
            metadata: Default::default(),
            body: None,
            body_file: None,
        };
        let sent = crate::transport::build_request(&definition, request)
            .await
            .unwrap();
        assert!(!sent
            .headers
            .keys()
            .any(|key| key.eq_ignore_ascii_case("authorization")
                || key.eq_ignore_ascii_case("x-api-key")));
        assert_eq!(
            sent.headers.get("x-private-key").is_some(),
            matches!(scheme, nora_ai::AuthScheme::ApiKeyHeader { .. })
        );
    }
}

#[tokio::test]
async fn custom_provider_authorizes_without_a_key_when_a_service_does_not_require_one() {
    let plugin = crate::BundledProvider::default();
    let mut value = manifest();
    value["baseURL"] = json!("http://localhost:11434/v1");
    plugin
        .initialize(InitializeParams {
            subscription_accounts_enabled: true,
            installed_manifest: None,
            runtime_config: serde_json::Value::Null,
            api_version: 1,
            plugin_id: "compatible".into(),
            plugin_directory: "/tmp/compatible".into(),
            data_directory: "/tmp/compatible".into(),
            manifest: value,
        })
        .await
        .unwrap();
    let result = plugin
        .authorize(json!({"method":"apiKey","inputs":{"apiKey":""}}))
        .await
        .unwrap();
    assert_eq!(result["credentials"]["type"], "apiKey");
    assert_eq!(result["credentials"]["access"], "");
    assert_eq!(result["ok"], true);
}

#[tokio::test]
async fn custom_provider_uses_the_configured_protocol_and_destination_without_vendor_defaults() {
    for (api, base, path, auth_header) in [
        (
            "openai-completions",
            "https://relay.invalid/v1",
            "/chat/completions",
            "Authorization",
        ),
        (
            "openai-responses",
            "https://relay.invalid/v1",
            "/responses",
            "Authorization",
        ),
        (
            "anthropic-messages",
            "https://relay.invalid",
            "/v1/messages",
            "x-api-key",
        ),
        (
            "google-generative-ai",
            "https://relay.invalid/v1beta",
            "/models/actual:generateContent",
            "query:key",
        ),
    ] {
        let mut value = manifest();
        value["baseURL"] = json!(base);
        value["api"] = json!(api);
        let definition = crate::definition::Definition::parse(value).unwrap();
        assert!(definition.models.is_empty());
        assert!(definition.config.catalog.is_empty());
        let request = nora_provider_plugin_sdk::FetchRequest {
            subscription_accounts_enabled: true,
            account: "account".into(),
            credentials: Some(json!({"type":"apiKey","access":"private-custom-key"})),
            network_proxy: None,
            url: format!("{base}{path}"),
            method: "POST".into(),
            headers: Default::default(),
            metadata: Default::default(),
            body: Some("eyJtb2RlbCI6ImFjdHVhbCJ9".into()),
            body_file: None,
        };
        let sent = crate::transport::build_request(&definition, request)
            .await
            .unwrap();
        if auth_header == "query:key" {
            let url = url::Url::parse(&sent.url).unwrap();
            assert_eq!(url.path(), format!("/v1beta{path}"));
            assert!(url
                .query_pairs()
                .any(|(key, value)| key == "key" && value == "private-custom-key"));
        } else {
            assert_eq!(sent.url, format!("{base}{path}"));
            assert!(
                sent.headers
                    .iter()
                    .any(|(name, value)| name.eq_ignore_ascii_case(auth_header)
                        && value.contains("private-custom-key")),
                "{api}: {auth_header}"
            );
        }
    }
}
