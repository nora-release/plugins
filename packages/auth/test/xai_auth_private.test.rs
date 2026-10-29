use super::*;
use async_trait::async_trait;
use nora_ai::http::{HttpClientError, HttpStreamingResponse};

struct HangingHttpClient;

#[async_trait]
impl HttpClient for HangingHttpClient {
    async fn stream(
        &self,
        _request: HttpRequest,
    ) -> Result<HttpStreamingResponse, HttpClientError> {
        unreachable!("timeout test uses a one-shot request")
    }

    async fn request(&self, _request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        std::future::pending().await
    }
}

#[tokio::test]
async fn auxiliary_requests_have_a_bounded_timeout() {
    let error = request_with_timeout(
        &HangingHttpClient,
        HttpRequest::new(HttpMethod::Get, "https://example.test"),
        "xAI test request",
        Duration::from_millis(1),
    )
    .await
    .unwrap_err();

    assert_eq!(
        error,
        OAuthError::Transport("xAI test request timed out".to_string())
    );
}

#[test]
fn default_client_version_satisfies_the_account_proxy_gate() {
    let version = XaiClientContext::default().version;
    let core = version.split('-').next().unwrap_or_default();
    let parts = core
        .split('.')
        .map(|part| part.parse::<u64>().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(version, xai_grok_client_version());
    assert_eq!(parts.len(), 3);
    assert!((parts[0], parts[1], parts[2]) >= (1, 0, 13));
}
