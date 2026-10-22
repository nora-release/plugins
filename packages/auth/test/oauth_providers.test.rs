use super::*;
use async_trait::async_trait;
use base64::Engine;
use nora_ai::http::{HttpClientError, HttpResponse, HttpStreamingResponse, ReqwestHttpClient};
use std::sync::Mutex;

#[derive(Default)]
struct StubHttpClient {
    response: Mutex<Option<HttpResponse>>,
    last_request: Mutex<Option<HttpRequest>>,
}

impl StubHttpClient {
    fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            response: Mutex::new(Some(HttpResponse {
                status,
                headers: BTreeMap::new(),
                body: body.into(),
            })),
            last_request: Mutex::new(None),
        }
    }

    fn last_request(&self) -> Option<HttpRequest> {
        self.last_request.lock().unwrap().clone()
    }
}

#[async_trait]
impl HttpClient for StubHttpClient {
    async fn stream(
        &self,
        _request: HttpRequest,
    ) -> Result<HttpStreamingResponse, HttpClientError> {
        unreachable!("OAuth providers use one-shot requests")
    }

    async fn request(&self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        *self.last_request.lock().unwrap() = Some(request);
        Ok(self.response.lock().unwrap().take().unwrap())
    }
}

fn credentials(provider: OAuthProviderId) -> OAuthCredentials {
    OAuthCredentials::new(
        provider,
        "old-access",
        Some("old-refresh".to_string()),
        Some(0),
        BTreeMap::from([("kept".to_string(), JsonValue::string("yes"))]),
    )
}

#[tokio::test]
async fn anthropic_refresh_posts_json_and_preserves_existing_refresh_token() {
    let client = StubHttpClient::new(
        200,
        br#"{"access_token":"new-access","expires_in":3600}"#.to_vec(),
    );
    let provider = AnthropicOAuthProvider::new("https://example.test/token", "client");

    let refreshed = provider
        .refresh(&credentials(OAuthProviderId::Anthropic), &client)
        .await
        .unwrap();

    assert_eq!(refreshed.access_token, "new-access");
    assert_eq!(refreshed.refresh_token.as_deref(), Some("old-refresh"));
    assert_eq!(
        refreshed
            .extras
            .get("kept")
            .and_then(|value| value.as_str()),
        Some("yes")
    );

    let request = client.last_request().unwrap();
    assert_eq!(request.method, HttpMethod::Post);
    assert_eq!(request.url, "https://example.test/token");
    assert_eq!(
        request.headers.get("content-type").map(String::as_str),
        Some("application/json")
    );
    let body = serde_json::from_slice::<Value>(request.body.as_deref().unwrap()).unwrap();
    assert_eq!(body["client_id"], "client");
    assert_eq!(body["refresh_token"], "old-refresh");
}

#[tokio::test]
async fn anthropic_refresh_uses_rotated_refresh_token_when_present() {
    let client = StubHttpClient::new(
        200,
        br#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}"#
            .to_vec(),
    );
    let provider = AnthropicOAuthProvider::new("https://example.test/token", "client");

    let refreshed = provider
        .refresh(&credentials(OAuthProviderId::Anthropic), &client)
        .await
        .unwrap();

    assert_eq!(refreshed.access_token, "new-access");
    assert_eq!(refreshed.refresh_token.as_deref(), Some("new-refresh"));
    assert!(refreshed.expires_at.unwrap() > nora_ai::oauth::now_millis());
}

#[tokio::test]
async fn openai_codex_refresh_posts_form_and_extracts_account_id() {
    let account_payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct_123","chatgpt_plan_type":"plus"},"https://api.openai.com/profile":{"email":"member@example.com","name":"Member Name"}}"#);
    let access_token = format!("header.{account_payload}.signature");
    let body = json!({
        "access_token": access_token,
        "refresh_token": "new-refresh",
        "expires_in": 7200,
    })
    .to_string();
    let client = StubHttpClient::new(200, body.into_bytes());
    let provider = OpenAICodexOAuthProvider::new("https://example.test/oauth", "client");

    let refreshed = provider
        .refresh(&credentials(OAuthProviderId::OpenAICodex), &client)
        .await
        .unwrap();

    assert_eq!(refreshed.refresh_token.as_deref(), Some("new-refresh"));
    assert_eq!(
        refreshed
            .extras
            .get("accountId")
            .and_then(|value| value.as_str()),
        Some("acct_123")
    );

    assert_eq!(
        refreshed.extras["email"].as_str(),
        Some("member@example.com")
    );
    assert_eq!(refreshed.extras["name"].as_str(), Some("Member Name"));
    assert_eq!(refreshed.extras["plan"].as_str(), Some("plus"));
    assert_eq!(refreshed.extras["kept"].as_str(), Some("yes"));

    let request = client.last_request().unwrap();
    assert_eq!(
        request.headers.get("content-type").map(String::as_str),
        Some("application/x-www-form-urlencoded")
    );
    assert_eq!(
        String::from_utf8(request.body.unwrap()).unwrap(),
        "client_id=client&grant_type=refresh_token&refresh_token=old-refresh"
    );
}

#[test]
fn openai_codex_extracts_account_id_from_padded_jwt_payload() {
    let account_payload = base64::engine::general_purpose::URL_SAFE
        .encode(br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct_padded"}}"#);
    let access_token = format!("header.{account_payload}.signature");

    assert_eq!(
        OpenAICodexOAuthProvider::extract_account_id_from_jwt(&access_token).as_deref(),
        Some("acct_padded")
    );
    assert_eq!(
        OpenAICodexOAuthProvider::extract_account_id_from_jwt("not-a-jwt"),
        None
    );
}

#[tokio::test]
async fn github_copilot_refresh_exchanges_pat_and_caches_endpoint() {
    let client = StubHttpClient::new(
        200,
        br#"{"token":"session-token","expires_at":2000,"endpoints":{"api":"https://copilot.test"}}"#.to_vec(),
    );
    let provider = GitHubCopilotOAuthProvider::default();

    let refreshed = provider
        .refresh(&credentials(OAuthProviderId::GitHubCopilot), &client)
        .await
        .unwrap();

    assert_eq!(refreshed.access_token, "session-token");
    assert_eq!(refreshed.refresh_token.as_deref(), Some("old-refresh"));
    assert_eq!(refreshed.expires_at, Some(2_000_000 - EARLY_REFRESH_MILLIS));
    assert_eq!(
        refreshed
            .extras
            .get("endpoint")
            .and_then(|value| value.as_str()),
        Some("https://copilot.test")
    );

    let request = client.last_request().unwrap();
    assert_eq!(request.method, HttpMethod::Get);
    assert_eq!(
        request.headers.get("authorization").map(String::as_str),
        Some("Bearer old-refresh")
    );
    assert_eq!(
        request
            .headers
            .get("copilot-integration-id")
            .map(String::as_str),
        Some("vscode-chat")
    );
}

#[tokio::test]
async fn github_copilot_refresh_defaults_expiry_when_missing() {
    let client = StubHttpClient::new(200, br#"{"token":"session-token"}"#.to_vec());
    let provider = GitHubCopilotOAuthProvider::default();
    let before = nora_ai::oauth::now_millis();

    let refreshed = provider
        .refresh(&credentials(OAuthProviderId::GitHubCopilot), &client)
        .await
        .unwrap();

    let after = nora_ai::oauth::now_millis();
    assert_eq!(refreshed.access_token, "session-token");
    let expires = refreshed.expires_at.unwrap();
    assert!(expires >= before + 20 * 60 * 1_000 - 1_000);
    assert!(expires <= after + 20 * 60 * 1_000);
    assert!(refreshed.extras.get("endpoint").is_none());
}

#[tokio::test]
async fn refresh_status_errors_are_reported_with_provider_prefix() {
    let client = StubHttpClient::new(401, b"denied".to_vec());
    let provider = AnthropicOAuthProvider::new("https://example.test/token", "client");

    let error = provider
        .refresh(&credentials(OAuthProviderId::Anthropic), &client)
        .await
        .unwrap_err();

    assert_eq!(
        error,
        OAuthError::RefreshFailed("anthropic 401: denied".to_string())
    );
}

#[test]
fn xai_default_refresh_endpoint_uses_oauth_issuer() {
    let provider = XaiOAuthProvider::default();

    assert_eq!(provider.token_url, "https://auth.x.ai/oauth2/token");
}

#[tokio::test]
async fn xai_refresh_sends_the_client_contract_and_preserves_rotated_tokens() {
    let client = StubHttpClient::new(
        200,
        br#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}"#
            .to_vec(),
    );
    let provider = XaiOAuthProvider::new("https://issuer.example/oauth2/token", "client-id");

    let refreshed = provider
        .refresh(&credentials(OAuthProviderId::XAI), &client)
        .await
        .unwrap();

    assert_eq!(refreshed.access_token, "new-access");
    assert_eq!(refreshed.refresh_token.as_deref(), Some("new-refresh"));
    assert_eq!(
        refreshed
            .extras
            .get("kept")
            .and_then(|value| value.as_value().as_str()),
        Some("yes")
    );

    let request = client.last_request().unwrap();
    assert_eq!(request.method, HttpMethod::Post);
    assert_eq!(request.url, "https://issuer.example/oauth2/token");
    assert_eq!(
        request.headers.get("content-type").map(String::as_str),
        Some("application/x-www-form-urlencoded")
    );
    assert_eq!(
        request
            .headers
            .get(XAI_CLIENT_VERSION_HEADER)
            .map(String::as_str),
        Some(xai_grok_client_version())
    );
    let body = String::from_utf8(request.body.unwrap()).unwrap();
    assert!(body.contains("client_id=client-id"));
    assert!(body.contains("grant_type=refresh_token"));
    assert!(body.contains("refresh_token=old-refresh"));
}

#[test]
fn default_provider_set_matches_nora_order() {
    let providers = default_oauth_providers();
    let ids = providers
        .iter()
        .map(|provider| provider.id().to_string())
        .collect::<Vec<_>>();

    assert_eq!(ids, ["anthropic", "openai-codex", "github-copilot", "xai"]);
}

#[test]
fn default_reqwest_client_satisfies_oauth_trait_bound() {
    fn accepts_client(_client: &dyn nora_ai::http::HttpClient) {}
    accepts_client(&ReqwestHttpClient::new());
}

#[test]
fn openai_profile_uses_identity_claims_without_guessing_from_workspace_ids() {
    let token = |value: Value| {
        format!(
            "header.{}.signature",
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&value).unwrap())
        )
    };
    let access = token(
        json!({"https://api.openai.com/auth":{"chatgpt_account_id":"workspace-123","chatgpt_plan_type":"pro"},"https://api.openai.com/profile":{"email":"access@example.com","name":"Access Name"}}),
    );
    let identity = token(json!({"email":"identity@example.com","name":"Identity Name"}));
    let profile = OpenAICodexOAuthProvider::profile_extras_from_tokens(&access, Some(&identity));
    assert_eq!(profile["email"].as_str(), Some("identity@example.com"));
    assert_eq!(profile["name"].as_str(), Some("Identity Name"));
    assert_eq!(profile["plan"].as_str(), Some("pro"));
    assert!(!profile.contains_key("accountId"));
    let workspace_only = token(
        json!({"https://api.openai.com/auth":{"chatgpt_account_id":"workspace-123"},"email":"workspace-123"}),
    );
    assert!(OpenAICodexOAuthProvider::profile_extras_from_tokens(&workspace_only, None).is_empty());
    assert!(
        OpenAICodexOAuthProvider::profile_extras_from_tokens("opaque-private-token", None)
            .is_empty()
    );
}

#[tokio::test]
async fn openai_refresh_reads_id_token_profile_and_preserves_missing_existing_profile_fields() {
    let id = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(br#"{"email":"renewed@example.com","name":"Renewed Name"}"#);
    let client = StubHttpClient::new(200, json!({"access_token":"opaque-renewed-access","id_token":format!("header.{id}.signature"),"expires_in":3600}).to_string().into_bytes());
    let mut previous = credentials(OAuthProviderId::OpenAICodex);
    previous
        .extras
        .insert("plan".into(), JsonValue::string("business"));
    let renewed = OpenAICodexOAuthProvider::default()
        .refresh(&previous, &client)
        .await
        .unwrap();
    assert_eq!(
        renewed.extras["email"].as_str(),
        Some("renewed@example.com")
    );
    assert_eq!(renewed.extras["name"].as_str(), Some("Renewed Name"));
    assert_eq!(renewed.extras["plan"].as_str(), Some("business"));
    assert!(!serde_json::to_string(&renewed.extras)
        .unwrap()
        .contains("signature"));
}
