use super::*;
use nora_ai::http::{HttpClientError, HttpResponse, HttpStreamingResponse};
use std::collections::VecDeque;
use std::sync::Mutex;

#[derive(Default)]
struct StubHttpClient {
    responses: Mutex<VecDeque<HttpResponse>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl StubHttpClient {
    fn push(&self, status: u16, body: impl Into<Vec<u8>>) {
        self.responses.lock().unwrap().push_back(HttpResponse {
            status,
            headers: BTreeMap::new(),
            body: body.into(),
        });
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl HttpClient for StubHttpClient {
    async fn stream(
        &self,
        _request: HttpRequest,
    ) -> Result<HttpStreamingResponse, HttpClientError> {
        unreachable!("xAI auth tests use one-shot requests")
    }

    async fn request(&self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| HttpClientError::Transport("missing stub response".to_string()))
    }
}

#[test]
fn default_proxy_headers_use_the_grok_contract_version() {
    let client = XaiClientContext::default();
    let inference = xai_oauth_inference_headers(&client);
    let auxiliary = xai_oauth_auxiliary_headers(&client);

    assert_eq!(client.version, xai_grok_client_version());
    assert_eq!(
        inference
            .get(XAI_CLIENT_VERSION_HEADER)
            .map(String::as_str),
        Some(xai_grok_client_version())
    );
    assert_eq!(
        auxiliary
            .get(XAI_CLIENT_VERSION_HEADER)
            .map(String::as_str),
        Some(xai_grok_client_version())
    );
}

#[test]
fn oauth_contract_forces_proxy_and_replaces_conflicting_headers() {
    let mut auth = ResolvedProviderAuth {
        token: Some("live-token".to_string()),
        credential_provider_id: None,
        scheme: AuthScheme::ApiKeyHeader {
            name: "x-api-key".to_string(),
        },
        headers: BTreeMap::from([
            ("Authorization".to_string(), "Bearer stale".to_string()),
            ("x-api-key".to_string(), "stale-key".to_string()),
            ("x-xai-token-auth".to_string(), "wrong".to_string()),
        ]),
        base_url: Some("https://api.x.ai/v1".to_string()),
        metadata: BTreeMap::new(),
    };

    apply_xai_oauth_session_contract_with_client(
        &mut auth,
        &XaiClientContext::new("1.2.3", "headless"),
    );

    assert_eq!(auth.scheme, AuthScheme::Bearer);
    assert_eq!(auth.base_url.as_deref(), Some(XAI_OAUTH_PROXY_BASE_URL));
    assert_eq!(auth.token.as_deref(), Some("live-token"));
    assert!(!auth
        .headers
        .keys()
        .any(|name| name.eq_ignore_ascii_case("authorization")));
    assert!(!auth
        .headers
        .keys()
        .any(|name| name.eq_ignore_ascii_case("x-api-key")));
    assert_eq!(
        auth.headers.get(XAI_TOKEN_AUTH_HEADER).map(String::as_str),
        Some(XAI_TOKEN_AUTH_VALUE)
    );
    assert_eq!(
        auth.headers
            .get(XAI_AUTHENTICATE_RESPONSE_HEADER)
            .map(String::as_str),
        Some(XAI_AUTHENTICATE_RESPONSE_VALUE)
    );
    assert_eq!(
        auth.headers
            .get(XAI_CLIENT_VERSION_HEADER)
            .map(String::as_str),
        Some("1.2.3")
    );
    assert_eq!(
        auth.headers.get(XAI_CLIENT_MODE_HEADER).map(String::as_str),
        Some("headless")
    );
}

#[test]
fn api_key_contract_forces_public_api_and_removes_session_headers() {
    let mut auth = xai_oauth_resolved_auth("oauth-token");
    auth.token = Some("api-key".to_string());

    apply_xai_api_key_contract(&mut auth);

    assert_eq!(auth.base_url.as_deref(), Some(XAI_API_BASE_URL));
    assert_eq!(auth.scheme, AuthScheme::Bearer);
    assert!(auth.headers.is_empty());
}

#[test]
fn api_key_contract_preserves_explicit_compatible_endpoint() {
    let credentials =
        OAuthCredentials::new(OAuthProviderId::ApiKey, "key", None, None, BTreeMap::new());
    let mut auth = ResolvedProviderAuth {
        token: Some("key".to_string()),
        credential_provider_id: None,
        scheme: AuthScheme::Bearer,
        headers: xai_oauth_session_headers(),
        base_url: Some("https://xai-compatible.example/v1".to_string()),
        metadata: BTreeMap::new(),
    };

    assert!(apply_xai_credentials_contract(
        "xai-api-key",
        &credentials,
        &mut auth
    ));

    assert_eq!(
        auth.base_url.as_deref(),
        Some("https://xai-compatible.example/v1")
    );
    assert!(auth.headers.is_empty());
}

#[tokio::test]
async fn user_validation_requires_verified_user_and_returns_profile_extras() {
    let client = StubHttpClient::default();
    client.push(
        200,
        br#"{"userId":"user-1","email":"person@example.com","firstName":"Ada","lastName":"Lovelace","teamId":"team-1"}"#,
    );

    let user = validate_xai_oauth_session(&client, "access-token")
        .await
        .unwrap();

    assert_eq!(user.user_id, "user-1");
    assert_eq!(user.display_name().as_deref(), Some("Ada Lovelace"));
    let extras = user.credential_extras();
    assert_eq!(
        extras
            .get("userId")
            .and_then(|value| value.as_value().as_str()),
        Some("user-1")
    );
    assert_eq!(
        extras
            .get("teamId")
            .and_then(|value| value.as_value().as_str()),
        Some("team-1")
    );
    assert_eq!(extras.get("accountId"), None);

    let request = client.requests().pop().unwrap();
    assert_eq!(request.method, HttpMethod::Get);
    assert_eq!(request.url, XAI_OAUTH_USER_URL);
    assert_eq!(
        request.headers.get("authorization").map(String::as_str),
        Some("Bearer access-token")
    );
    assert_eq!(
        request
            .headers
            .get(XAI_TOKEN_AUTH_HEADER)
            .map(String::as_str),
        Some(XAI_TOKEN_AUTH_VALUE)
    );
    assert!(request.headers.contains_key(XAI_CLIENT_VERSION_HEADER));
    assert!(request.headers.contains_key(XAI_CLIENT_MODE_HEADER));
    assert!(!request
        .headers
        .contains_key(XAI_AUTHENTICATE_RESPONSE_HEADER));
}

#[tokio::test]
async fn user_validation_rejects_unauthorized_and_missing_user_id() {
    let unauthorized = StubHttpClient::default();
    unauthorized.push(401, br#"{"error":"unauthorized"}"#);
    assert_eq!(
        validate_xai_oauth_session(&unauthorized, "bad-token")
            .await
            .unwrap_err(),
        OAuthError::RefreshFailed("xAI OAuth session verification returned status 401".to_string())
    );

    let missing_user = StubHttpClient::default();
    missing_user.push(200, br#"{"email":"person@example.com"}"#);
    assert_eq!(
        validate_xai_oauth_session(&missing_user, "token")
            .await
            .unwrap_err(),
        OAuthError::InvalidResponse("xAI /user response missing userId".to_string())
    );
}

#[tokio::test]
async fn user_validation_accepts_grok_principal_fallbacks() {
    for (body, expected) in [
        (br#"{"principalType":"Team","principalId":"team-42"}"#.as_slice(), "team-42"),
        (br#"{"sub":"user-sub"}"#.as_slice(), "user-sub"),
        (br#"{"user_id":"snake-user"}"#.as_slice(), "snake-user"),
        (br#"{"teamId":"team-7","organizationId":"org-9"}"#.as_slice(), "team-7"),
    ] {
        let client = StubHttpClient::default();
        client.push(200, body);
        let user = validate_xai_oauth_session(&client, "token").await.unwrap();
        assert_eq!(user.user_id, expected);
    }
}

#[tokio::test]
async fn oauth_and_api_key_model_discovery_use_separate_hosts_and_headers() {
    let oauth = StubHttpClient::default();
    oauth.push(200, br#"{"userId":"user-1"}"#);
    oauth.push(
        200,
        br#"{"data":[{"id":"grok-4.5","apiBackend":"responses"}]}"#,
    );

    let models = fetch_xai_oauth_models(&oauth, "oauth-token").await.unwrap();

    assert_eq!(models.data.len(), 1);
    let requests = oauth.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].url, XAI_OAUTH_MODELS_URL);
    assert_eq!(
        requests[1].headers.get("x-userid").map(String::as_str),
        Some("user-1")
    );
    assert_eq!(
        requests[1]
            .headers
            .get(XAI_TOKEN_AUTH_HEADER)
            .map(String::as_str),
        Some(XAI_TOKEN_AUTH_VALUE)
    );
    assert!(!requests[1]
        .headers
        .contains_key(XAI_AUTHENTICATE_RESPONSE_HEADER));

    let api_key = StubHttpClient::default();
    api_key.push(200, br#"{"data":[{"id":"grok-4"}]}"#);
    fetch_xai_api_key_models(&api_key, "xai-key").await.unwrap();
    let request = api_key.requests().pop().unwrap();
    assert_eq!(request.url, XAI_API_MODELS_URL);
    assert_eq!(request.headers.len(), 2);
    assert_eq!(
        request.headers.get("authorization").map(String::as_str),
        Some("Bearer xai-key")
    );
}

#[tokio::test]
async fn billing_uses_proxy_contract_and_decodes_credits() {
    let client = StubHttpClient::default();
    client.push(
        200,
        br#"{"config":{"creditUsagePercent":12.5,"prepaidBalance":{"val":4200},"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","end":"2026-08-10T00:00:00Z"}}}"#,
    );

    let billing = fetch_xai_oauth_billing(&client, "oauth-token", "user-1")
        .await
        .unwrap();

    let config = billing.config.unwrap();
    assert_eq!(config.credit_usage_percent, Some(12.5));
    assert_eq!(config.prepaid_balance.unwrap().val, 4200);
    let request = client.requests().pop().unwrap();
    assert_eq!(request.url, XAI_OAUTH_BILLING_URL);
    assert_eq!(
        request.headers.get("x-userid").map(String::as_str),
        Some("user-1")
    );
    assert!(!request
        .headers
        .contains_key(XAI_AUTHENTICATE_RESPONSE_HEADER));
}

#[tokio::test]
async fn model_discovery_resolves_user_id_from_user_endpoint_when_omitted() {
    let client = StubHttpClient::default();
    client.push(200, br#"{"principalId":"team-9"}"#);
    client.push(
        200,
        br#"{"data":[{"id":"grok-4.5","apiBackend":"responses"}]}"#,
    );

    let models = fetch_xai_oauth_models_for_user(&client, "oauth-token", "")
        .await
        .unwrap();

    assert_eq!(models.data.len(), 1);
    let requests = client.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].url, XAI_OAUTH_USER_URL);
    assert_eq!(requests[1].url, XAI_OAUTH_MODELS_URL);
    assert_eq!(
        requests[1].headers.get("x-userid").map(String::as_str),
        Some("team-9")
    );
}

#[tokio::test]
async fn billing_resolves_user_id_from_user_endpoint_when_omitted() {
    let client = StubHttpClient::default();
    client.push(200, br#"{"userId":"user-1"}"#);
    client.push(
        200,
        br#"{"config":{"creditUsagePercent":1.5,"prepaidBalance":{"val":99}}}"#,
    );

    let billing = fetch_xai_oauth_billing(&client, "oauth-token", " ")
        .await
        .unwrap();

    let config = billing.config.unwrap();
    assert_eq!(config.credit_usage_percent, Some(1.5));
    assert_eq!(config.prepaid_balance.unwrap().val, 99);
    let requests = client.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].url, XAI_OAUTH_USER_URL);
    assert_eq!(requests[1].url, XAI_OAUTH_BILLING_URL);
    assert_eq!(
        requests[1].headers.get("x-userid").map(String::as_str),
        Some("user-1")
    );
}

#[tokio::test]
async fn refresh_preserves_user_id_so_model_discovery_keeps_working() {
    let client = StubHttpClient::default();
    client.push(
        200,
        br#"{"access_token":"rotated-token","refresh_token":"rotated-refresh","expires_in":3600}"#,
    );
    client.push(
        200,
        br#"{"data":[{"id":"grok-4.5","apiBackend":"responses"}]}"#,
    );

    let base = OAuthCredentials::new(
        OAuthProviderId::XAI,
        "old-access",
        Some("old-refresh".to_string()),
        Some(0),
        BTreeMap::from([("userId".to_string(), JsonValue::string("user-1"))]),
    );
    let provider = XaiOAuthProvider::new("https://issuer.example/oauth2/token", "client-id");
    let refreshed = provider.refresh(&base, &client).await.unwrap();
    let user_id = refreshed
        .extras
        .get("userId")
        .and_then(|value| value.as_value().as_str())
        .expect("refresh must preserve the persisted xAI userId");

    let models = fetch_xai_oauth_models_for_user(&client, &refreshed.access_token, user_id)
        .await
        .unwrap();
    assert_eq!(models.data.len(), 1);

    let requests = client.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].url, "https://issuer.example/oauth2/token");
    assert_eq!(requests[1].url, XAI_OAUTH_MODELS_URL);
    assert_eq!(
        requests[1].headers.get("x-userid").map(String::as_str),
        Some("user-1")
    );
}
