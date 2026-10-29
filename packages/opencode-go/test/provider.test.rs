use super::*;
#[path = "../../quota/test/provider_fixture.rs"]
mod fixture;

#[tokio::test]
async fn official_usage_protocol_and_private_endpoint_boundaries() {
    let payload: Value = serde_json::from_str(include_str!("fixtures/usage.json")).unwrap();
    fixture::verify(
        &OpenCodeGo,
        "https://opencode.ai/zen/go",
        "https://opencode.ai/zen/go/v1/usage",
        true,
        &payload,
    )
    .await;
}

#[tokio::test]
async fn usage_queries_carry_the_required_opencode_gateway_session() {
    let payload: Value = serde_json::from_str(include_str!("fixtures/usage.json")).unwrap();
    let client = fixture::Client::json(&payload);
    let auth = ResolvedProviderAuth {
        token: Some("private-key".into()),
        base_url: Some("https://opencode.ai/zen/go".into()),
        ..Default::default()
    };
    OpenCodeGo.usage(&auth, None, &client).await.unwrap();
    let requests = client.requests.lock().unwrap();
    let session = &requests[0].headers["x-opencode-session"];
    assert_eq!(requests[0].headers["x-opencode-session-id"], *session);
    assert_eq!(session.len(), 30);
    assert!(session.starts_with("ses_"));
    let low = u16::from_str_radix(&session[13..16], 16).unwrap();
    assert!(0xfff - low < 16);
    assert!(session[16..]
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric()));
}
