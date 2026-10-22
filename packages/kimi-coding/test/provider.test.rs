use super::*;
#[path = "../../quota/test/provider_fixture.rs"]
mod fixture;

#[tokio::test]
async fn official_usage_protocol_and_private_endpoint_boundaries() {
    let payload: Value = serde_json::from_str(include_str!("fixtures/usage.json")).unwrap();
    fixture::verify(
        &KimiCoding,
        "https://api.kimi.com/coding",
        "https://api.kimi.com/coding/v1/usages",
        true,
        &payload,
    )
    .await;
}

#[tokio::test]
async fn the_other_official_kimi_host_keeps_its_own_usage_query() {
    let payload: Value = serde_json::from_str(include_str!("fixtures/usage.json")).unwrap();
    fixture::verify(
        &KimiCoding,
        "https://api.kimi.ai/coding",
        "https://api.kimi.ai/coding/v1/usages",
        true,
        &payload,
    )
    .await;
}
