use super::*;
#[path = "fixture.rs"]
mod fixture;
fn tokens() -> session::Tokens {
    session::Tokens {
        access: "private-access".into(),
        refresh: "private-refresh".into(),
        expires: now() + 3600000,
        ..Default::default()
    }
}
#[tokio::test]
async fn an_existing_project_and_paid_entitlements_are_read_from_load_code_assist() {
    let client = fixture::Client::new(vec![fixture::Reply::json(
        json!({"cloudaicompanionProject":{"id":"project-a"},"currentTier":{"id":"standard-tier","name":"Standard"},"paidTier":{"id":"g1-pro-tier","name":"Google AI Pro"}}),
    )]);
    let project = load(App::Antigravity, &tokens(), &client).await.unwrap();
    assert_eq!(project.id, "project-a");
    assert_eq!(project.plan, "Google AI Pro");
    assert_eq!(
        project.entitlements(),
        vec!["g1-pro-tier", "standard-tier", ""]
    );
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].url, format!("{PROD}/v1internal:loadCodeAssist"));
}
#[tokio::test]
async fn antigravity_onboarding_uses_daily_snake_case_and_reports_no_project() {
    let client = fixture::Client::new(vec![
        fixture::Reply::json(json!({"allowedTiers":[{"id":"free-tier","isDefault":true}]})),
        fixture::Reply::json(
            json!({"done":true,"response":{"cloudaicompanionProject":{"id":"generated"}}}),
        ),
    ]);
    let project = load(App::Antigravity, &tokens(), &client).await.unwrap();
    assert_eq!(project.id, "generated");
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        requests[1].url,
        format!("{}/v1internal:onboardUser", App::Antigravity.base())
    );
    let body: Value = serde_json::from_slice(requests[1].body.as_ref().unwrap()).unwrap();
    assert_eq!(body["tier_id"], "free-tier");
    assert_eq!(body["metadata"]["ide_type"], "ANTIGRAVITY");
    assert_eq!(requests[1].headers["x-goog-api-client"], "gl-node/22.21.1");
}
#[tokio::test]
async fn enterprise_gemini_requires_an_explicit_project_without_guessing_one() {
    let client = fixture::Client::new(vec![fixture::Reply::json(
        json!({"allowedTiers":[{"id":"standard-tier","isDefault":true,"userDefinedCloudaicompanionProject":true}]}),
    )]);
    assert!(load(App::GeminiCLI, &tokens(), &client)
        .await
        .unwrap_err()
        .message
        .contains("project"));
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}
