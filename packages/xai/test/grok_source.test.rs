use super::*;
#[path = "grok_fixture.rs"]
mod fixture;
use fixture::Fixture;

#[cfg(unix)]
#[tokio::test]
async fn temporary_source_and_renewal_failures_keep_the_account_connected() {
    let provider = super::super::Xai::default();
    for failure in ["torn", "unreadable", "cli-failed", "no-cli", "expired"] {
        let fixture = Fixture::new();
        match failure {
            "torn" => std::fs::write(&fixture.file, "{").unwrap(),
            "unreadable" => std::fs::create_dir(&fixture.file).unwrap(),
            _ => fixture.write("private-expired-token", "account-a", true),
        }
        if failure == "cli-failed" {
            fixture.cli("echo private-cli-stderr >&2; exit 1");
        } else if failure == "expired" {
            fixture.cli("exit 0");
        }
        let credentials = fixture.credentials();
        let before = fixture.saved();
        let refresh = provider
            .refresh_source(&credentials, false, None)
            .await
            .unwrap_err();
        let resolve = provider
            .resolve_source(&credentials, None)
            .await
            .unwrap_err();
        for error in [refresh, resolve] {
            assert_eq!(error.code, "upstream_transport_error", "{failure}: {error}");
            assert!(!error.message.contains("private-"));
            assert!(!error.message.contains("Reconnect"));
        }
        assert_eq!(
            nora_provider::auth::encode(&credentials, "xai", true),
            before
        );
    }
}

#[tokio::test]
async fn confirmed_signout_or_replaced_account_requires_authentication() {
    let provider = super::super::Xai::default();
    for complete in [
        None,
        Some(serde_json::json!({})),
        Some(Fixture::record("other", "account-b", false)),
        Some(
            serde_json::json!({"https://auth.x.ai":{"key":"private-invalid","user_id":"account-a","oidc_issuer":"https://foreign.invalid"}}),
        ),
    ] {
        let fixture = Fixture::new();
        if let Some(complete) = complete {
            std::fs::write(&fixture.file, complete.to_string()).unwrap();
        }
        let credentials = fixture.credentials();
        assert_eq!(
            provider
                .refresh_source(&credentials, false, None)
                .await
                .unwrap_err()
                .code,
            "authentication_required"
        );
        assert_eq!(
            provider
                .resolve_source(&credentials, None)
                .await
                .unwrap_err()
                .code,
            "authentication_required"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn unsuccessful_forced_renewal_is_retryable_even_with_a_future_expiration() {
    let provider = super::super::Xai::default();
    let fixture = Fixture::new();
    fixture.cli("exit 0");
    fixture.write("rejected-token", "account-a", false);
    assert_eq!(
        provider
            .refresh_source(&fixture.credentials(), true, None)
            .await
            .unwrap_err()
            .code,
        "upstream_transport_error"
    );
}
