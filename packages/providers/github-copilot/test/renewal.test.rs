use super::*;
use nora_ai::{HttpClientError, HttpRequest, HttpResponse, HttpStreamingResponse};
use serde_json::json;

struct Client {
    status: u16,
    malformed: bool,
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, _: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        unreachable!()
    }
    async fn request(&self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        if self.status == 0 {
            return Err(HttpClientError::Transport("secret-network-detail".into()));
        }
        let body = if self.malformed {
            b"invalid-secret-response".to_vec()
        } else if self.status != 200 {
            br#"{"private":"must-not-escape"}"#.to_vec()
        } else if request.url.ends_with("/v2/token") {
            serde_json::to_vec(&json!({"token":"new-session","expires_at":4_000_000_000_i64}))
                .unwrap()
        } else {
            serde_json::to_vec(&json!({"id":1,"login":"octocat"})).unwrap()
        };
        Ok(HttpResponse {
            status: self.status,
            headers: BTreeMap::new(),
            body,
        })
    }
}
fn credentials() -> OAuthCredentials {
    let mut credentials = OAuthCredentials::new(
        OAuthProviderId::GitHubCopilot,
        "old-session",
        Some("saved-account-token".into()),
        Some(0),
        BTreeMap::new(),
    );
    copilot_identity::apply_profile(&mut credentials, &json!({"id":1,"login":"octocat"}), "")
        .unwrap();
    credentials
}
#[tokio::test]
async fn temporary_exchange_and_identity_errors_are_retryable_and_keep_the_same_saved_account() {
    let provider = Copilot::default();
    for (status, malformed) in [
        (0, false),
        (429, false),
        (500, false),
        (502, false),
        (503, false),
        (504, false),
        (200, true),
    ] {
        let mut account = credentials();
        let saved = account.clone();
        let client = Client { status, malformed };
        let error = provider.refresh(&account, &client).await.unwrap_err();
        assert_eq!(error.code, "upstream_transport_error");
        assert!(error.message.contains("Credentials were kept"));
        assert!(!error.message.contains("secret"));
        assert!(!error.message.contains("Sign in again"));
        assert_eq!(account, saved);
        account.extras.remove(copilot_identity::IDENTITY_KEY);
        let error = provider
            .enrich_account(&mut account, &client, None)
            .await
            .unwrap_err();
        assert_eq!(error.code, "upstream_transport_error");
        let renewed = provider
            .refresh(
                &saved,
                &Client {
                    status: 200,
                    malformed: false,
                },
            )
            .await
            .unwrap();
        assert_eq!(renewed.access_token, "new-session");
        assert_eq!(renewed.refresh_token, saved.refresh_token);
        assert_eq!(renewed.extras, saved.extras);
    }
}
#[tokio::test]
async fn only_explicit_401_and_403_refusals_ask_for_sign_in_again() {
    let provider = Copilot::default();
    for status in [401, 403] {
        let account = credentials();
        let saved = account.clone();
        let client = Client {
            status,
            malformed: false,
        };
        let error = provider.refresh(&account, &client).await.unwrap_err();
        assert_eq!(error.code, "authentication_required");
        assert!(error.message.contains("Sign in again"));
        assert_eq!(account, saved);
    }
    assert_eq!(
        source_error(copilot_editors::EditorsError::Unavailable).code,
        "upstream_transport_error"
    );
    assert_eq!(
        source_error(copilot_editors::EditorsError::InvalidFile).code,
        "upstream_transport_error"
    );
    assert_eq!(
        source_error(copilot_editors::EditorsError::Rejected).code,
        "authentication_required"
    );
}
