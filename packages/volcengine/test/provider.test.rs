use super::*;
use crate::signing;
use nora_ai::{HttpClientError, HttpRequest, HttpStreamingResponse, OAuthProviderId};
use serde_json::json;
use std::{collections::BTreeMap, sync::Mutex};

struct Client {
    requests: Mutex<Vec<HttpRequest>>,
    status: u16,
    payload: Value,
}
impl Client {
    fn new(payload: Value) -> Self {
        Self {
            requests: Mutex::new(vec![]),
            status: 200,
            payload,
        }
    }
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        Ok(HttpStreamingResponse {
            status: self.status,
            headers: BTreeMap::new(),
            body: Box::pin(futures::stream::iter(vec![Ok(self
                .payload
                .to_string()
                .into_bytes()
                .into())])),
        })
    }
}
fn configured() -> (OAuthCredentials, ResolvedProviderAuth) {
    let mut credentials = OAuthCredentials::api_key(OAuthProviderId::ApiKey, "inference-secret");
    Volcengine
        .configure_api_credentials(
            &json!({"accessKeyID":"AKLTexample","secretAccessKey":"control-secret"}),
            &mut credentials,
        )
        .unwrap();
    let mut auth = ResolvedProviderAuth {
        token: Some(credentials.access_token.clone()),
        base_url: Some("https://ark.cn-shanghai.volces.com/api/plan/v3".into()),
        ..Default::default()
    };
    Volcengine.enrich_auth(&credentials, &mut auth);
    (credentials, auth)
}
#[tokio::test]
async fn only_control_plane_receives_private_access_keys_and_exact_signed_protocol() {
    let (_, auth) = configured();
    assert_eq!(auth.token.as_deref(), Some("inference-secret"));
    assert!(auth.headers.is_empty());
    let client =
        Client::new(json!({"Result":{"Tier":"Lite","AFPFiveHour":{"Quota":100,"Used":42}}}));
    let value = Volcengine.usage(&auth, None, &client).await.unwrap();
    assert_eq!(value["items"][0]["usedPercent"], 42.0);
    assert!(!value.to_string().contains("secret"));
    assert!(!value.to_string().contains("AKLTexample"));
    let requests = client.requests.lock().unwrap();
    let request = &requests[0];
    assert_eq!(requests.len(), 1);
    assert_eq!(
        request.url,
        "https://open.volcengineapi.com/?Action=GetAFPUsage&Version=2024-01-01"
    );
    assert_eq!(request.method, nora_ai::HttpMethod::Post);
    assert_eq!(request.body.as_deref(), Some(b"{}".as_slice()));
    assert_eq!(request.headers["content-type"], "application/json");
    let at = chrono::NaiveDateTime::parse_from_str(&request.headers["x-date"], "%Y%m%dT%H%M%SZ")
        .unwrap()
        .and_utc();
    let (expected, _, sha) = signing::sign(
        "AKLTexample",
        "control-secret",
        "cn-shanghai",
        &signing::query("GetAFPUsage"),
        b"{}",
        at,
    );
    assert_eq!(request.headers["authorization"], expected);
    assert_eq!(request.headers["x-content-sha256"], sha);
    assert!(!request
        .headers
        .values()
        .any(|value| value.contains("inference-secret") || value.contains("control-secret")));
}
#[tokio::test]
async fn optional_or_half_keys_and_other_endpoints_never_make_control_plane_calls() {
    let (_, mut auth) = configured();
    let client = Client::new(json!({}));
    for base in [
        "https://relay.invalid/api/plan/v3",
        "https://ark.cn-beijing.volces.com/api/v3",
        "http://ark.cn-beijing.volces.com/api/coding",
    ] {
        auth.base_url = Some(base.into());
        assert_eq!(
            Volcengine.usage(&auth, None, &client).await.unwrap()["available"],
            false
        );
    }
    auth.base_url = Some("https://ark.cn-beijing.volces.com/api/coding".into());
    for key in ["accessKeyID", "secretAccessKey"] {
        let mut copy = auth.clone();
        copy.metadata.remove(key);
        let value = Volcengine.usage(&copy, None, &client).await.unwrap();
        assert_eq!(value["available"], false);
        assert!(value["message"]
            .as_str()
            .unwrap()
            .contains("AccessKey ID and Secret"));
    }
    let mut credentials = OAuthCredentials::api_key(OAuthProviderId::ApiKey, "inference");
    Volcengine
        .configure_api_credentials(&json!({"accessKeyID":"one-half"}), &mut credentials)
        .unwrap();
    assert!(client.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn permission_errors_explain_the_cloud_key_without_expiring_the_inference_key() {
    let (_, auth) = configured();
    let client = Client::new(
        json!({"ResponseMetadata":{"Error":{"Code":"SignatureDoesNotMatch","Message":"The request signature does not match"}}}),
    );
    let error = Volcengine.usage(&auth, None, &client).await.unwrap_err();
    assert!(error.contains("SignatureDoesNotMatch"));
    assert!(error.contains("AccessKey ID and Secret"));
    assert!(!error.contains("inference-secret"));
    assert!(!error.contains("control-secret"));
    assert_eq!(auth.token.as_deref(), Some("inference-secret"));
}
#[test]
fn secret_fields_validate_types_and_headers_without_becoming_identity_fields() {
    for input in [
        json!({"accessKeyID":23}),
        json!({"secretAccessKey":"bad\r\nheader"}),
        json!({"secretAccessKey":"x".repeat(4097)}),
    ] {
        let mut credentials = OAuthCredentials::api_key(OAuthProviderId::ApiKey, "inference");
        assert!(Volcengine
            .configure_api_credentials(&input, &mut credentials)
            .is_err());
    }
    let (credentials, auth) = configured();
    assert!(credentials.extras.get("email").is_none());
    assert!(credentials.extras.get("name").is_none());
    assert_eq!(auth.metadata["accessKeyID"].as_str(), Some("AKLTexample"));
}
