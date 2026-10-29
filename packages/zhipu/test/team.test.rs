use super::*;
use serde_json::json;
#[path = "../../quota/test/provider_fixture.rs"]
mod fixture;

#[tokio::test]
async fn explicit_team_accounts_use_their_quota_kind_and_project_headers() {
    let mut credentials =
        OAuthCredentials::api_key(nora_ai::OAuthProviderId::ApiKey, "private-key");
    configure(
        &json!({"organizationID":"organization-a","projectID":"project-a"}),
        &mut credentials,
    )
    .unwrap();
    let mut auth = ResolvedProviderAuth {
        token: Some("private-key".into()),
        base_url: Some("https://api.z.ai/api/coding/paas/v4".into()),
        ..Default::default()
    };
    enrich(&credentials, &mut auth);
    let client = fixture::Client::json(
        &json!({"success":true,"data":{"limits":[{"type":"CREDIT_LIMIT","unit":3,"number":5,"percentage":50}]}}),
    );
    let result = usage(
        &auth,
        "api.z.ai",
        "https://api.z.ai/api/monitor/usage/quota/limit",
        &client,
    )
    .await
    .unwrap();
    assert_eq!(result["account"]["plan"], "GLM Coding Team");
    assert_eq!(result["items"][0]["countsForRouting"], true);
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        requests[0].url,
        "https://api.z.ai/api/monitor/usage/quota/limit?type=2"
    );
    assert_eq!(requests[0].headers["authorization"], "private-key");
    assert_eq!(
        requests[0].headers["Bigmodel-Organization"],
        "organization-a"
    );
    assert_eq!(requests[0].headers["Bigmodel-Project"], "project-a");
}

#[test]
fn half_configured_teams_are_rejected_without_exposing_credentials() {
    let mut credentials =
        OAuthCredentials::api_key(nora_ai::OAuthProviderId::ApiKey, "private-key");
    assert!(configure(
        &json!({"organizationID":"organization-a"}),
        &mut credentials
    )
    .is_err());
    assert!(configure(
        &json!({"organizationID":"bad\r\norganization","projectID":"project-a"}),
        &mut credentials
    )
    .is_err());
}

#[tokio::test]
async fn team_credentials_on_a_custom_origin_never_query_official_services() {
    let auth = ResolvedProviderAuth {
        token: Some("private-key".into()),
        base_url: Some("https://unrelated.invalid/v1".into()),
        metadata: std::collections::BTreeMap::from([
            (
                "glmOrganizationID".into(),
                JsonValue::string("organization-a"),
            ),
            ("glmProjectID".into(), JsonValue::string("project-a")),
        ]),
        ..Default::default()
    };
    let client = fixture::Client::json(&json!({}));
    assert_eq!(
        usage(
            &auth,
            "open.bigmodel.cn",
            "https://open.bigmodel.cn/api/monitor/usage/quota/limit",
            &client
        )
        .await
        .unwrap()["available"],
        false
    );
    assert!(client.requests.lock().unwrap().is_empty());
}
