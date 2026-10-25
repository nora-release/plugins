use super::*;
use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use nora_ai::http::{HttpClientError, HttpResponse, HttpStreamingResponse};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpStream as StdTcpStream;
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
        unreachable!("OAuth login tests use one-shot requests")
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
fn anthropic_authorize_url_contains_pkce_state_and_scope() {
    let pkce = OAuthPkce::new("verifier");
    let url =
        OAuthLogin::anthropic_authorize_url(&pkce, "http://localhost:53692/callback").unwrap();
    let params = query_params(&url);

    assert_eq!(
        params.get("client_id").map(String::as_str),
        Some(ANTHROPIC_CLIENT_ID)
    );
    assert_eq!(params.get("state").map(String::as_str), Some("verifier"));
    assert_eq!(params.get("code").map(String::as_str), Some("true"));
    assert_eq!(
        params.get("response_type").map(String::as_str),
        Some("code")
    );
    assert_eq!(
        params.get("code_challenge").map(String::as_str),
        Some(pkce.challenge.as_str())
    );
    assert_eq!(
        params.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    assert_eq!(
        params.get("redirect_uri").map(String::as_str),
        Some("http://localhost:53692/callback")
    );
    assert!(params
        .get("scope")
        .unwrap()
        .contains("user:sessions:claude_code"));
}

#[test]
fn openai_codex_authorize_url_contains_state_and_pkce() {
    let pkce = OAuthPkce::new("verifier");
    let url = OAuthLogin::openai_codex_authorize_url(
        &pkce,
        "state",
        "http://localhost:1455/auth/callback",
        "nora-test",
    )
    .unwrap();
    let params = query_params(&url);

    assert_eq!(
        params.get("client_id").map(String::as_str),
        Some(OPENAI_CODEX_CLIENT_ID)
    );
    assert_eq!(params.get("state").map(String::as_str), Some("state"));
    assert_eq!(
        params.get("scope").map(String::as_str),
        Some("openid profile email offline_access api.connectors.read api.connectors.invoke")
    );
    assert_eq!(
        params.get("code_challenge").map(String::as_str),
        Some(pkce.challenge.as_str())
    );
    assert_eq!(
        params.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    assert_eq!(
        params.get("redirect_uri").map(String::as_str),
        Some("http://localhost:1455/auth/callback")
    );
    assert_eq!(
        params.get("id_token_add_organizations").map(String::as_str),
        Some("true")
    );
    assert_eq!(
        params.get("codex_cli_simplified_flow").map(String::as_str),
        Some("true")
    );
    assert_eq!(
        params.get("originator").map(String::as_str),
        Some("nora-test")
    );
}

#[test]
fn xai_authorize_url_uses_oauth_issuer_not_accounts_home() {
    let pkce = OAuthPkce::new("verifier");
    let url =
        OAuthLogin::xai_authorize_url(&pkce, "state", "http://127.0.0.1:56121/callback").unwrap();
    let parsed = url::Url::parse(&url).unwrap();
    let params = query_params(&url);

    assert_eq!(parsed.origin().ascii_serialization(), "https://auth.x.ai");
    assert_eq!(parsed.path(), "/oauth2/authorize");
    assert_eq!(
        params.get("client_id").map(String::as_str),
        Some("b1a00492-073a-47ea-816f-4c329264a828")
    );
    assert_eq!(params.get("state").map(String::as_str), Some("state"));
    let scope = params.get("scope").unwrap();
    for required in [
        "openid",
        "offline_access",
        "grok-cli:access",
        "api:access",
        "conversations:read",
        "conversations:write",
        "workspaces:read",
        "workspaces:write",
    ] {
        assert!(scope.split_whitespace().any(|scope| scope == required));
    }
    assert_eq!(
        params.get("code_challenge").map(String::as_str),
        Some(pkce.challenge.as_str())
    );
    assert_eq!(
        params.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    assert_eq!(
        params.get("redirect_uri").map(String::as_str),
        Some("http://127.0.0.1:56121/callback")
    );
    assert!(params.get("nonce").is_some_and(|nonce| !nonce.is_empty()));
    assert_eq!(params.get("referrer").map(String::as_str), Some("nora"));
    assert_eq!(params.get("plan"), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn xai_login_verifies_user_and_persists_profile_extras() {
    let client = StubHttpClient::default();
    client.push(200, xai_discovery_response());
    client.push(
        200,
        format!(
            r#"{{"access_token":"{}","refresh_token":"xai-refresh","expires_in":3600}}"#,
            xai_team_access_token("team-1")
        ),
    );
    client.push(
        200,
        br#"{"userId":"user-1","email":"person@example.com","firstName":"Ada","lastName":"Lovelace","teamId":"team-1"}"#,
    );

    let credentials = OAuthLogin::login_xai(xai_loopback_callbacks(), &client)
        .await
        .unwrap();

    assert_eq!(credentials.provider, OAuthProviderId::XAI);
    assert!(credentials.access_token.contains('.'));
    assert_eq!(credentials.refresh_token.as_deref(), Some("xai-refresh"));
    assert_eq!(
        credentials
            .extras
            .get("userId")
            .and_then(|value| value.as_value().as_str()),
        Some("user-1")
    );
    assert_eq!(
        credentials
            .extras
            .get("name")
            .and_then(|value| value.as_value().as_str()),
        Some("Ada Lovelace")
    );
    assert_eq!(
        credentials
            .extras
            .get("teamId")
            .and_then(|value| value.as_value().as_str()),
        Some("team-1")
    );

    let requests = client.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[0].url,
        "https://auth.x.ai/.well-known/openid-configuration"
    );
    assert_eq!(requests[1].url, "https://auth.x.ai/oauth2/token");
    assert_eq!(
        requests[1]
            .headers
            .get(XAI_CLIENT_VERSION_HEADER)
            .map(String::as_str),
        Some(xai_grok_client_version())
    );
    let token_body = String::from_utf8(requests[1].body.clone().unwrap()).unwrap();
    assert!(token_body.contains("code_verifier="));
    assert!(!token_body.contains("code_challenge="));
    assert_eq!(requests[2].url, XAI_OAUTH_USER_URL);
    assert_eq!(
        requests[2]
            .headers
            .get(XAI_TOKEN_AUTH_HEADER)
            .map(String::as_str),
        Some(XAI_TOKEN_AUTH_VALUE)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn xai_login_rejects_token_when_user_endpoint_returns_unauthorized() {
    let client = StubHttpClient::default();
    client.push(200, xai_discovery_response());
    client.push(
        200,
        format!(
            r#"{{"access_token":"{}","refresh_token":"xai-refresh","expires_in":3600}}"#,
            xai_team_access_token("team-1")
        ),
    );
    client.push(401, br#"{"error":"unauthorized"}"#);

    let error = OAuthLogin::login_xai(xai_loopback_callbacks(), &client)
        .await
        .unwrap_err();

    assert_eq!(
        error,
        OAuthError::RefreshFailed("xAI account proxy rejected the OAuth session".to_string())
    );
}

#[tokio::test]
async fn github_device_flow_polls_and_exchanges_pat_for_session_token() {
    let client = StubHttpClient::default();
    client.push(
        200,
        br#"{"user_code":"ABCD","device_code":"dev","verification_uri":"https://github.test/login","interval":0}"#.to_vec(),
    );
    client.push(200, br#"{"error":"authorization_pending"}"#.to_vec());
    client.push(200, br#"{"access_token":"pat"}"#.to_vec());
    client.push(
        200,
        br#"{"token":"session","expires_at":2000,"endpoints":{"api":"https://api.test"}}"#.to_vec(),
    );
    client.push(200, br#"{"id":42,"login":"octocat"}"#.to_vec());

    let credentials = OAuthLogin::login_github_copilot_with_config(
        GitHubCopilotLoginConfig {
            client_id: "client".to_string(),
            device_code_url: "https://github.test/device".to_string(),
            access_token_url: "https://github.test/token".to_string(),
            copilot_token_url: "https://copilot.test/token".to_string(),
            github_user_url: "https://github.test/user".to_string(),
            github_host: String::new(),
            timeout: Duration::from_secs(1),
        },
        OAuthLoginCallbacks::silent(),
        &client,
    )
    .await
    .unwrap();

    assert_eq!(credentials.provider, OAuthProviderId::GitHubCopilot);
    assert_eq!(credentials.access_token, "session");
    assert_eq!(credentials.refresh_token.as_deref(), Some("pat"));
    assert_eq!(
        credentials
            .extras
            .get("endpoint")
            .and_then(|value| value.as_str()),
        Some("https://api.test")
    );
    let requests = client.requests();
    assert_eq!(requests.len(), 5);
    assert_eq!(requests[0].url, "https://github.test/device");
    assert_eq!(requests[0].method, HttpMethod::Post);
    assert_eq!(
        requests[0].headers.get("accept").map(String::as_str),
        Some("application/json")
    );
    assert_eq!(
        String::from_utf8(requests[0].body.clone().unwrap()).unwrap(),
        "client_id=client&scope=read%3Auser"
    );
    assert_eq!(requests[1].url, "https://github.test/token");
    assert_eq!(
        String::from_utf8(requests[1].body.clone().unwrap()).unwrap(),
        "client_id=client&device_code=dev&grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"
    );
    assert_eq!(requests[3].url, "https://copilot.test/token");
    assert_eq!(requests[4].url, "https://github.test/user");
    assert_eq!(credentials.extras["name"].as_str(), Some("octocat"));
    assert_eq!(
        credentials.extras["accountIdentity"].as_value(),
        &json!({"issuer":"https://github.com","subject":"42"})
    );
}

#[tokio::test]
async fn github_device_flow_surfaces_terminal_poll_errors() {
    let client = StubHttpClient::default();
    client.push(
        200,
        br#"{"user_code":"ABCD","device_code":"dev","verification_uri":"https://github.test/login","interval":0}"#.to_vec(),
    );
    client.push(200, br#"{"error":"access_denied"}"#.to_vec());

    let error = OAuthLogin::login_github_copilot_with_config(
        GitHubCopilotLoginConfig {
            client_id: "client".to_string(),
            device_code_url: "https://github.test/device".to_string(),
            access_token_url: "https://github.test/token".to_string(),
            copilot_token_url: "https://copilot.test/token".to_string(),
            github_user_url: "https://github.test/user".to_string(),
            github_host: String::new(),
            timeout: Duration::from_secs(1),
        },
        OAuthLoginCallbacks::silent(),
        &client,
    )
    .await
    .unwrap_err();

    assert_eq!(
        error,
        OAuthError::RefreshFailed("copilot device flow: access_denied".to_string())
    );
}

#[tokio::test]
async fn enable_copilot_models_posts_policy_and_keeps_going_on_errors() {
    let client = StubHttpClient::default();
    client.push(200, b"ok".to_vec());
    client.push(403, b"no".to_vec());

    OAuthLogin::enable_copilot_models(
        "session",
        "https://api.test/",
        &["gpt-5".to_string(), "claude".to_string()],
        OAuthLoginCallbacks::silent(),
        &client,
    )
    .await;

    let requests = client.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].url, "https://api.test/models/gpt-5/policy");
    assert_eq!(
        requests[0].headers.get("authorization").map(String::as_str),
        Some("Bearer session")
    );
    assert_eq!(
        requests[0].body.as_deref(),
        Some(&br#"{"state":"enabled"}"#[..])
    );
    assert_eq!(requests[1].url, "https://api.test/models/claude/policy");
}

fn query_params(url: &str) -> BTreeMap<String, String> {
    url::Url::parse(url)
        .unwrap()
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn xai_loopback_callbacks() -> OAuthLoginCallbacks {
    OAuthLoginCallbacks::new(
        |auth_url| {
            let auth_url = url::Url::parse(auth_url).unwrap();
            let params = auth_url
                .query_pairs()
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect::<BTreeMap<_, _>>();
            let redirect_uri = params.get("redirect_uri").unwrap().clone();
            let state = params.get("state").unwrap().clone();
            std::thread::spawn(move || {
                let callback = url::Url::parse(&redirect_uri).unwrap();
                let host = callback.host_str().unwrap();
                let port = callback.port().unwrap();
                let mut stream = StdTcpStream::connect((host, port)).unwrap();
                let request = format!(
                    "GET {}?code=test-code&state={} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
                    callback.path(), state, host
                );
                stream.write_all(request.as_bytes()).unwrap();
                let mut response = String::new();
                stream.read_to_string(&mut response).unwrap();
                assert!(response.contains("200 OK"));
            });
        },
        |_| {},
    )
}

fn xai_discovery_response() -> Vec<u8> {
    br#"{
        "issuer":"https://auth.x.ai",
        "authorization_endpoint":"https://auth.x.ai/oauth2/authorize",
        "token_endpoint":"https://auth.x.ai/oauth2/token",
        "jwks_uri":"https://auth.x.ai/oauth2/jwks",
        "id_token_signing_alg_values_supported":["RS256","ES256"]
    }"#
    .to_vec()
}

fn xai_team_access_token(team_id: &str) -> String {
    let payload = serde_json::json!({
        "principal_type": "Team",
        "principal_id": team_id,
        "team_id": team_id,
    });
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
    format!("e30.{payload}.signature")
}

#[tokio::test(flavor = "multi_thread")]
async fn codex_login_keeps_email_name_and_plan_without_saving_raw_identity_tokens() {
    let access_claims = URL_SAFE_NO_PAD.encode(br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"workspace-id","chatgpt_plan_type":"plus"}}"#);
    let identity_claims =
        URL_SAFE_NO_PAD.encode(br#"{"email":"signed-in@example.com","name":"Signed In Name"}"#);
    let client = StubHttpClient::default();
    client.push(200, serde_json::to_vec(&json!({"access_token":format!("header.{access_claims}.signature"),"id_token":format!("header.{identity_claims}.signature"),"refresh_token":"private-refresh","expires_in":3600})).unwrap());
    let credentials = tokio::time::timeout(
        Duration::from_secs(5),
        OAuthLogin::login_openai_codex(xai_loopback_callbacks(), &client),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        credentials.extras["accountId"].as_str(),
        Some("workspace-id")
    );
    assert_eq!(
        credentials.extras["email"].as_str(),
        Some("signed-in@example.com")
    );
    assert_eq!(credentials.extras["name"].as_str(), Some("Signed In Name"));
    assert_eq!(credentials.extras["plan"].as_str(), Some("plus"));
    assert_eq!(client.requests().len(), 1);
    assert!(!serde_json::to_string(&credentials.extras)
        .unwrap()
        .contains("signature"));
}
