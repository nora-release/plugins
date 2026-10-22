use super::*;
#[path = "provider_fixture.rs"]
mod fixture;

#[tokio::test]
async fn custom_or_insecure_origins_never_receive_the_key() {
    let client = fixture::Client::json(&json!({}));
    for base in [
        "https://relay.invalid/v1",
        "http://api.example.test",
        "https://api.example.test.evil",
        "https://private-user:private-password@api.example.test",
        "https://api.example.test:8443",
    ] {
        let auth = ResolvedProviderAuth {
            token: Some("private-key".into()),
            base_url: Some(base.into()),
            ..Default::default()
        };
        assert!(fetch(
            &auth,
            "api.example.test",
            "https://api.example.test/usage",
            true,
            &client
        )
        .await
        .unwrap()
        .is_none());
    }
    assert!(client.requests.lock().unwrap().is_empty());
}

#[test]
fn missing_and_nonfinite_balances_are_unknown_but_zero_is_preserved() {
    for value in [json!(null), json!("NaN"), json!("Infinity")] {
        assert!(balance_at(&json!({"amount":value}), "/amount", "USD").is_err());
    }
    let result = balance_at(&json!({"amount":"0"}), "/amount", "USD").unwrap();
    assert_eq!(result["balances"][0]["amount"], 0.0);
    assert!(result["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn an_explicit_target_must_match_the_declared_secure_host() {
    let client = fixture::Client::json(&json!({}));
    let auth = ResolvedProviderAuth {
        token: Some("private-key".into()),
        base_url: Some("https://api.example.test/v1".into()),
        ..Default::default()
    };
    for target in [
        "http://account.example.test/usage",
        "https://unrelated.invalid/usage",
        "https://private:secret@account.example.test/usage",
        "https://account.example.test:8443/usage",
    ] {
        assert_eq!(
            fetch_between(
                &auth,
                "api.example.test",
                "account.example.test",
                target,
                true,
                &[],
                &client
            )
            .await
            .unwrap_err(),
            QuotaError::InvalidResponse
        );
    }
    assert!(client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn missing_credentials_and_header_newlines_never_make_a_request() {
    let client = fixture::Client::json(&json!({}));
    for token in [None, Some(""), Some("private\r\nkey")] {
        let auth = ResolvedProviderAuth {
            token: token.map(str::to_string),
            base_url: Some("https://api.example.test/v1".into()),
            ..Default::default()
        };
        assert_eq!(
            fetch(
                &auth,
                "api.example.test",
                "https://api.example.test/usage",
                true,
                &client
            )
            .await
            .unwrap_err(),
            QuotaError::Credentials
        );
    }
    assert!(client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_oversized_body_is_rejected_even_when_its_prefix_is_valid_json() {
    let mut body = br#"{"balance":20}"#.to_vec();
    body.resize(1024 * 1024 + 1, b' ');
    let client = fixture::Client {
        requests: std::sync::Mutex::new(Vec::new()),
        status: 200,
        body,
    };
    let auth = ResolvedProviderAuth {
        token: Some("private-key".into()),
        base_url: Some("https://api.example.test/v1".into()),
        ..Default::default()
    };
    assert_eq!(
        fetch(
            &auth,
            "api.example.test",
            "https://api.example.test/usage",
            true,
            &client
        )
        .await
        .unwrap_err(),
        QuotaError::InvalidResponse
    );
}
