use super::*;
#[path = "../../quota/test/provider_fixture.rs"]
mod fixture;

#[tokio::test]
async fn official_usage_protocol_and_private_endpoint_boundaries() {
    let payload: Value = serde_json::from_str(include_str!("fixtures/usage.json")).unwrap();
    fixture::verify_with_request_count(
        &Zhipu,
        "https://open.bigmodel.cn/api/coding/paas/v4",
        "https://open.bigmodel.cn/api/monitor/usage/quota/limit",
        false,
        &payload,
        3,
    )
    .await;
}

#[tokio::test]
async fn personal_usage_adapter_reports_a_successful_empty_reset_response_as_zero() {
    use futures::stream;
    use nora_ai::{HttpClientError, HttpRequest, HttpStreamingResponse};
    use serde_json::json;
    use std::collections::BTreeMap;

    struct Client;
    #[async_trait]
    impl HttpClient for Client {
        async fn stream(
            &self,
            request: HttpRequest,
        ) -> Result<HttpStreamingResponse, HttpClientError> {
            let body = if request.url.ends_with("/api/monitor/usage/quota/limit") {
                include_bytes!("fixtures/usage.json").to_vec()
            } else if request
                .url
                .ends_with("/api/biz/customer-package-reset/list?targetType=PERSONAL")
            {
                br#"{"code":200,"success":true,"data":{"fiveHourResets":[],"weekResets":[]}}"#
                    .to_vec()
            } else if request.url.ends_with("/api/biz/subscription/list") {
                br#"{"code":200,"success":true,"data":[]}"#.to_vec()
            } else {
                panic!("Unexpected personal usage endpoint: {}", request.url);
            };
            Ok(HttpStreamingResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: Box::pin(stream::iter(vec![Ok(body.into())])),
            })
        }
    }
    let auth = ResolvedProviderAuth {
        token: Some("private-glm-key".into()),
        base_url: Some("https://open.bigmodel.cn/api/coding/paas/v4".into()),
        ..Default::default()
    };
    let usage = Zhipu.usage(&auth, None, &Client).await.unwrap();
    assert_eq!(
        usage["resetCredits"],
        json!({"available":true,"totalAvailable":0,"grants":[]})
    );
    assert_eq!(usage["available"], true);
}
