use super::*;
#[path = "../../quota/test/provider_fixture.rs"]
mod fixture;

#[tokio::test]
async fn official_usage_protocol_and_private_endpoint_boundaries() {
    let payload: Value = serde_json::from_str(include_str!("fixtures/usage.json")).unwrap();
    fixture::verify(
        &AiHubMix,
        "https://aihubmix.com/v1",
        "https://aihubmix.com/dashboard/billing/remain",
        true,
        &payload,
    )
    .await;
}

#[tokio::test]
async fn explicit_account_balance_tokens_are_never_used_for_inference() {
    let mut credentials =
        nora_ai::OAuthCredentials::api_key(nora_ai::OAuthProviderId::ApiKey, "inference-secret");
    AiHubMix
        .configure_api_credentials(
            &serde_json::json!({"balanceToken":"balance-secret"}),
            &mut credentials,
        )
        .unwrap();
    let mut auth = ResolvedProviderAuth {
        token: Some("inference-secret".into()),
        base_url: Some("https://aihubmix.com/v1".into()),
        ..Default::default()
    };
    AiHubMix.enrich_auth(&credentials, &mut auth);
    let client =
        fixture::Client::json(&serde_json::json!({"success":true,"data":{"quota":2500000}}));
    let result = AiHubMix.usage(&auth, None, &client).await.unwrap();
    assert_eq!(auth.token.as_deref(), Some("inference-secret"));
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].url, "https://aihubmix.com/api/user/self");
    assert_eq!(
        requests[0].headers["authorization"],
        "Bearer balance-secret"
    );
    assert!(!result.to_string().contains("secret"));
}
