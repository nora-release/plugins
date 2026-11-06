use super::*;
#[test]
fn oauth_credentials_preserve_their_provider_and_public_identity() {
    for (provider, id) in [
        (OAuthProviderId::OpenAICodex, "openai-codex"),
        (OAuthProviderId::Anthropic, "anthropic"),
        (OAuthProviderId::GitHubCopilot, "github-copilot"),
        (OAuthProviderId::XAI, "xai"),
    ] {
        let credentials = OAuthCredentials::new(
            provider.clone(),
            "private-access",
            Some("private-refresh".into()),
            Some(123),
            BTreeMap::from([("email".into(), JsonValue::string("person@example.com"))]),
        );
        let value = encode(&credentials, id, false);
        assert_eq!(value["email"], "person@example.com");
        assert_eq!(
            value["credentialProvider"],
            serde_json::to_value(provider).unwrap()
        );
        assert_eq!(value["type"], "oauth");
        assert_eq!(value["refresh"], "private-refresh");
    }
}

#[test]
fn source_references_never_save_external_tokens_or_their_expiry() {
    let credentials = OAuthCredentials::new(
        OAuthProviderId::XAI,
        "stale-external-access",
        Some("stale-external-refresh".into()),
        Some(0),
        BTreeMap::from([(
            "noraAccountSource".into(),
            JsonValue::from(json!({"kind":"grokCLI"})),
        )]),
    );
    let value = encode(&credentials, "xai", true);
    assert_eq!(value["access"], "");
    assert!(value["refresh"].is_null());
    assert!(value["expires"].is_null());
    assert_eq!(value["type"], "grokCLI");
    assert!(!value.to_string().contains("stale-external"));
}

#[test]
fn api_accounts_keep_their_authentication_type_and_readable_name() {
    let credentials = OAuthCredentials::api_key(OAuthProviderId::ApiKey, "private-api-key");
    let value = encode(&credentials, "openai-api-key", false);
    assert!(is_api(&value));
    assert_eq!(value["type"], "apiKey");
    assert_eq!(value["authMethod"], "apiKey");
    assert_eq!(value["label"], "API key");
    assert!(!value["label"].as_str().unwrap().contains("private-api-key"));

    let mut named = credentials;
    named
        .extras
        .insert("name".into(), JsonValue::string("Work"));
    assert_eq!(encode(&named, "openai-api-key", false)["label"], "Work");
}

pub(crate) fn resolved_provider_auth_from_credentials(
    provider_id: &str,
    credentials: &OAuthCredentials,
) -> ResolvedProviderAuth {
    let mut auth = base_auth(provider_id, credentials);
    crate::test_adapter(provider_id).enrich_auth(credentials, &mut auth);
    auth
}
