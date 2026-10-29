use super::*;
use nora_ai::{HttpClientError, HttpRequest, HttpResponse, HttpStreamingResponse};
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};

struct Client {
    reply: Mutex<Option<Result<HttpResponse, HttpClientError>>>,
    requests: AtomicUsize,
}

impl Client {
    fn reply(status: u16, body: Value) -> Self {
        Self {
            reply: Mutex::new(Some(Ok(HttpResponse {
                status,
                headers: BTreeMap::new(),
                body: body.to_string().into_bytes(),
            }))),
            requests: AtomicUsize::new(0),
        }
    }
    fn disconnected() -> Self {
        Self {
            reply: Mutex::new(Some(Err(HttpClientError::Transport(
                "private-network-diagnostic".into(),
            )))),
            requests: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, _: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        unreachable!()
    }
    async fn request(&self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        assert_eq!(request.url, "https://auth.openai.com/oauth/token");
        assert!(String::from_utf8(request.body.unwrap())
            .unwrap()
            .contains("grant_type=refresh_token"));
        self.requests.fetch_add(1, Ordering::Relaxed);
        self.reply.lock().unwrap().take().unwrap()
    }
}

fn account(refresh: &str) -> OAuthCredentials {
    OAuthCredentials::new(
        OAuthProviderId::OpenAICodex,
        "private-valid-access",
        Some(refresh.into()),
        Some(nora_ai::oauth::now_millis() + 3_600_000),
        BTreeMap::from([("accountId".into(), json!("workspace-a").into())]),
    )
}

#[tokio::test]
async fn temporary_owned_refresh_failures_keep_valid_credentials_and_require_only_a_retry() {
    for client in [
        Client::reply(
            403,
            json!({"error":"invalid_grant","private":"private-response-diagnostic"}),
        ),
        Client::reply(429, json!({"error":"invalid_grant"})),
        Client::reply(503, json!({"error":"invalid_grant"})),
        Client::reply(408, json!({"error":"server_error"})),
        Client::reply(200, json!({"private":"private-malformed-response"})),
        Client::reply(
            200,
            json!({"access_token":"","refresh_token":"private-rotated-token","expires_in":3600}),
        ),
        Client::disconnected(),
    ] {
        let credentials = account("private-refresh");
        let before = credentials.clone();
        let error = OpenAI.refresh(&credentials, &client).await.unwrap_err();
        assert_eq!(error.code, "upstream_transport_error");
        assert!(!error.message.contains("private-"));
        assert!(!error.message.to_lowercase().contains("sign in"));
        assert_eq!(credentials, before);
        assert_eq!(client.requests.load(Ordering::Relaxed), 1);
    }
}

#[tokio::test]
async fn only_refused_or_missing_owned_refresh_credentials_require_sign_in() {
    for status in [400, 401] {
        let client = Client::reply(
            status,
            json!({"error":"invalid_grant","private":"private-refused-token"}),
        );
        let credentials = account("private-refused-refresh");
        let error = OpenAI.refresh(&credentials, &client).await.unwrap_err();
        assert_eq!(error.code, "authentication_required");
        assert!(!error.message.contains("private-"));
        assert_eq!(
            credentials.refresh_token.as_deref(),
            Some("private-refused-refresh")
        );
    }
    for refresh in [None, Some("".into()), Some("   ".into())] {
        let client = Client::reply(200, json!({}));
        let mut credentials = account("unused");
        credentials.refresh_token = refresh;
        assert_eq!(
            OpenAI
                .refresh(&credentials, &client)
                .await
                .unwrap_err()
                .code,
            "authentication_required"
        );
        assert_eq!(client.requests.load(Ordering::Relaxed), 0);
    }
}

#[tokio::test]
async fn owned_refresh_recovers_after_a_temporary_failure_or_a_new_sign_in() {
    for status in [400, 429, 503] {
        let credentials = account("private-old-refresh");
        assert!(OpenAI
            .refresh(
                &credentials,
                &Client::reply(status, json!({"error":"invalid_grant"}))
            )
            .await
            .is_err());
        let credentials = if status == 400 {
            account("private-new-login")
        } else {
            credentials
        };
        let renewed = OpenAI.refresh(&credentials, &Client::reply(200, json!({"access_token":"private-new-access","refresh_token":"private-new-refresh","expires_in":3600}))).await.unwrap();
        assert_eq!(renewed.access_token, "private-new-access");
        assert_eq!(
            renewed.refresh_token.as_deref(),
            Some("private-new-refresh")
        );
        assert_eq!(renewed.extras, credentials.extras);
        assert!(renewed.expires_at.unwrap() > nora_ai::oauth::now_millis());
    }
}

#[tokio::test]
async fn empty_optional_tokens_do_not_replace_the_existing_refresh_grant() {
    let credentials = account("private-kept-refresh");
    let renewed = OpenAI
        .refresh(
            &credentials,
            &Client::reply(200, json!({"access_token":"private-renewed-access","refresh_token":"","id_token":"","expires_in":3600})),
        )
        .await
        .unwrap();
    assert_eq!(renewed.refresh_token, credentials.refresh_token);
    assert_eq!(renewed.extras, credentials.extras);
    assert_eq!(renewed.access_token, "private-renewed-access");
}

#[tokio::test]
async fn concurrent_owned_accounts_do_not_share_refusal_or_renewed_credentials() {
    let old = account("private-refused-refresh");
    let mut other = account("private-other-refresh");
    other
        .extras
        .insert("accountId".into(), json!("workspace-b").into());
    let rejected = Client::reply(401, json!({"error":"invalid_grant"}));
    let renewed = Client::reply(
        200,
        json!({"access_token":"private-other-access","refresh_token":"private-other-rotated","expires_in":3600}),
    );
    let (rejected, renewed) = tokio::join!(
        OpenAI.refresh(&old, &rejected),
        OpenAI.refresh(&other, &renewed)
    );
    assert_eq!(rejected.unwrap_err().code, "authentication_required");
    let renewed = renewed.unwrap();
    assert_eq!(renewed.access_token, "private-other-access");
    assert_eq!(renewed.extras["accountId"].as_str(), Some("workspace-b"));
}
