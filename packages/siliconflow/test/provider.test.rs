use super::*;
#[path = "../../quota/test/provider_fixture.rs"]
mod fixture;

#[tokio::test]
async fn official_usage_protocol_and_private_endpoint_boundaries() {
    let payload: Value = serde_json::from_str(include_str!("fixtures/usage.json")).unwrap();
    fixture::verify(
        &SiliconFlow,
        "https://api.siliconflow.cn/v1",
        "https://api.siliconflow.cn/v1/user/info",
        true,
        &payload,
    )
    .await;
}
