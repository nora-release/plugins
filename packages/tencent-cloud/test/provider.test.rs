use super::*;
use crate::signing;
use nora_ai::{HttpClientError, HttpRequest, HttpStreamingResponse, OAuthProviderId};
use serde_json::json;
use std::{collections::BTreeMap, sync::Mutex};

struct Client {
    requests: Mutex<Vec<HttpRequest>>,
    handler: fn(&HttpRequest) -> (u16, Value),
}
impl Client {
    fn new(handler: fn(&HttpRequest) -> (u16, Value)) -> Self {
        Self {
            requests: Mutex::new(vec![]),
            handler,
        }
    }
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        let (status, payload) = (self.handler)(&request);
        self.requests.lock().unwrap().push(request);
        Ok(HttpStreamingResponse {
            status,
            headers: BTreeMap::new(),
            body: Box::pin(futures::stream::iter(vec![Ok(payload
                .to_string()
                .into_bytes()
                .into())])),
        })
    }
}
fn configured(base: &str) -> (OAuthCredentials, ResolvedProviderAuth) {
    let mut credentials = OAuthCredentials::api_key(OAuthProviderId::ApiKey, "sk-plan-inference");
    TencentCloud
        .configure_api_credentials(
            &json!({"accessKeyID":"AKIDexample","secretAccessKey":"control-secret"}),
            &mut credentials,
        )
        .unwrap();
    let mut auth = ResolvedProviderAuth {
        token: Some(credentials.access_token.clone()),
        base_url: Some(base.into()),
        ..Default::default()
    };
    TencentCloud.enrich_auth(&credentials, &mut auth);
    (credentials, auth)
}
fn one_team(request: &HttpRequest) -> (u16, Value) {
    match request.headers["x-tc-action"].as_str() {
        "DescribeTokenPlanList" => (
            200,
            json!({"Response":{"TokenPlanSet":[{"TeamId":"team-one","Name":"My enterprise","ProductType":"enterprise","Status":"enable"}],"TotalCount":1}}),
        ),
        "DescribeTokenPlan" => (
            200,
            json!({"Response":{"TeamId":"team-one","PackageInfo":{"CycleQuota":"1000","TotalUsed":"100"},"TokenSummary":{"CycleStartTime":"2026-04-01T00:00:00+08:00","CycleEndTime":"2026-05-01T00:00:00+08:00"}}}),
        ),
        action => panic!("Unexpected Tencent action: {action}"),
    }
}
#[tokio::test]
async fn real_list_and_detail_contract_use_only_the_cloud_access_key_and_selected_region() {
    for (base, region) in [
        ("https://tokenhub.tencentmaas.com/plan/v3", "ap-guangzhou"),
        (
            "https://tokenhub-intl.tencentmaas.com/plan/anthropic",
            "ap-singapore",
        ),
    ] {
        let (_, auth) = configured(base);
        let client = Client::new(one_team);
        assert!(auth.headers.is_empty());
        assert_eq!(auth.token.as_deref(), Some("sk-plan-inference"));
        let value = TencentCloud.usage(&auth, None, &client).await.unwrap();
        assert_eq!(value["account"]["plan"], "Token Plan Enterprise");
        assert_eq!(value["items"][0]["usedPercent"], 10.0);
        assert_eq!(value["items"][0]["unit"], "credits");
        assert!(!value.to_string().contains("secret"));
        assert!(!value.to_string().contains("AKIDexample"));
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        for request in requests.iter() {
            assert_eq!(request.url, "https://tokenhub.tencentcloudapi.com/");
            assert_eq!(request.method, nora_ai::HttpMethod::Post);
            assert_eq!(request.headers["x-tc-version"], "2026-03-22");
            assert_eq!(request.headers["x-tc-region"], region);
            assert_eq!(
                request.headers["content-type"],
                "application/json; charset=utf-8"
            );
            let seconds = request.headers["x-tc-timestamp"].parse::<i64>().unwrap();
            let at = chrono::DateTime::from_timestamp(seconds, 0).unwrap();
            let expected = signing::sign(
                "AKIDexample",
                "control-secret",
                "tokenhub",
                signing::HOST,
                &request.headers["x-tc-action"],
                request.body.as_ref().unwrap(),
                at,
            );
            assert_eq!(request.headers["authorization"], expected);
            assert!(!request.headers.values().any(
                |value| value.contains("sk-plan-inference") || value.contains("control-secret")
            ));
        }
        assert_eq!(
            serde_json::from_slice::<Value>(requests[0].body.as_ref().unwrap()).unwrap(),
            json!({"Limit":100,"Offset":0})
        );
        assert_eq!(
            serde_json::from_slice::<Value>(requests[1].body.as_ref().unwrap()).unwrap(),
            json!({"TeamId":"team-one"})
        );
    }
}
#[tokio::test]
async fn multiple_teams_keep_list_totals_when_a_detail_is_missing_or_fails() {
    let (_, auth) = configured("https://tokenhub-intl.tencentmaas.com/plan/v3");
    let client = Client::new(|request| {
        if request.headers["x-tc-action"] == "DescribeTokenPlanList" {
            return (
                200,
                json!({"Response":{"TokenPlanSet":[
                    {"TeamId":"team-alpha","Name":"alpha","ProductType":"enterprise-auto","Status":"enable","PackageInfo":{"TotalQuota":"5000","TotalUsed":"1250"}},
                    {"TeamId":"team-beta","Name":"beta","Status":"isolate","StopReason":"arrears"}
                ]}}),
            );
        }
        let params: Value = serde_json::from_slice(request.body.as_ref().unwrap()).unwrap();
        if params["TeamId"] == "team-alpha" {
            (200, json!({"Response":{"Name":"alpha","Status":"enable"}}))
        } else {
            (
                503,
                json!({"Response":{"Error":{"Code":"InternalError","Message":"unavailable"}}}),
            )
        }
    });
    let value = TencentCloud.usage(&auth, None, &client).await.unwrap();
    assert_eq!(value["items"].as_array().unwrap().len(), 2);
    assert_eq!(value["items"][0]["label"], "alpha · Month");
    assert_eq!(value["items"][0]["usedPercent"], 25.0);
    assert_eq!(value["items"][0]["unit"], "tokens");
    assert!(value["items"][1]["value"]
        .as_str()
        .unwrap()
        .contains("beta · isolate, arrears"));
    assert_eq!(value["items"][1]["countsForRouting"], false);
}
#[tokio::test]
async fn missing_cloud_keys_and_non_enterprise_origins_make_no_requests() {
    let (_, mut auth) = configured("https://tokenhub.tencentmaas.com/plan/v3");
    let client = Client::new(|_| panic!("No cloud request expected"));
    for key in ["accessKeyID", "secretAccessKey"] {
        let mut copy = auth.clone();
        copy.metadata.remove(key);
        let value = TencentCloud.usage(&copy, None, &client).await.unwrap();
        assert_eq!(value["available"], false);
        assert!(value["message"]
            .as_str()
            .unwrap()
            .contains("SecretId and SecretKey"));
    }
    for base in [
        "https://relay.example/plan/v3",
        "https://tokenhub.tencentmaas.com/v1",
        "https://api.lkeap.cloud.tencent.com/plan/v3",
    ] {
        auth.base_url = Some(base.into());
        assert_eq!(
            TencentCloud.usage(&auth, None, &client).await.unwrap()["available"],
            false
        );
    }
    assert!(client.requests.lock().unwrap().is_empty());
    let mut credentials = OAuthCredentials::api_key(OAuthProviderId::ApiKey, "inference");
    TencentCloud
        .configure_api_credentials(&json!({"accessKeyID":"one-half"}), &mut credentials)
        .unwrap();
}
#[tokio::test]
async fn iam_refusals_and_absent_plans_are_errors_without_expiring_inference() {
    let (_, auth) = configured("https://tokenhub.tencentmaas.com/plan/v3");
    let client = Client::new(|_| {
        (
            200,
            json!({"Response":{"Error":{"Code":"AuthFailure.SignatureFailure","Message":"The provided credentials could not be validated"}}}),
        )
    });
    let error = TencentCloud.usage(&auth, None, &client).await.unwrap_err();
    assert!(error.contains("AuthFailure.SignatureFailure"));
    assert!(error.contains("SecretId and SecretKey"));
    assert!(!error.contains("control-secret"));
    assert!(!error.contains("sk-plan-inference"));
    let empty = Client::new(|_| (200, json!({"Response":{"TokenPlanSet":[]}})));
    assert!(TencentCloud
        .usage(&auth, None, &empty)
        .await
        .unwrap_err()
        .contains("no Token Plan Enterprise in ap-guangzhou"));
    assert_eq!(auth.token.as_deref(), Some("sk-plan-inference"));
}
#[test]
fn cloud_secrets_are_validated_and_stay_out_of_identity_and_inference_headers() {
    for inputs in [
        json!({"accessKeyID":false}),
        json!({"secretAccessKey":"key\nheader"}),
        json!({"secretAccessKey":"x".repeat(4097)}),
    ] {
        let mut credentials = OAuthCredentials::api_key(OAuthProviderId::ApiKey, "inference");
        assert!(TencentCloud
            .configure_api_credentials(&inputs, &mut credentials)
            .is_err());
    }
    let (credentials, auth) = configured("https://tokenhub.tencentmaas.com/plan/v3");
    assert!(credentials.extras.get("email").is_none());
    assert!(credentials.extras.get("name").is_none());
    assert_eq!(
        auth.metadata["secretAccessKey"].as_str(),
        Some("control-secret")
    );
    assert!(auth.headers.is_empty());
}
