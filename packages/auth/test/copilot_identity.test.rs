use super::*;
use async_trait::async_trait;
use nora_ai::{
    http::{HttpClientError, HttpResponse, HttpStreamingResponse},
    OAuthProviderId,
};
use std::{collections::BTreeMap, sync::Mutex};

struct Client {
    status: u16,
    value: Value,
    requests: Mutex<Vec<HttpRequest>>,
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, _: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        unreachable!()
    }
    async fn request(&self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        Ok(HttpResponse {
            status: self.status,
            headers: BTreeMap::new(),
            body: serde_json::to_vec(&self.value).unwrap(),
        })
    }
}
fn credentials() -> OAuthCredentials {
    OAuthCredentials::new(
        OAuthProviderId::GitHubCopilot,
        "short-session",
        Some("long-account-token".into()),
        None,
        BTreeMap::new(),
    )
}

#[tokio::test]
async fn github_user_id_is_verified_with_the_account_token_and_survives_login_renames() {
    let client = Client {
        status: 200,
        value: json!({"id":42,"login":"Octocat","email":null}),
        requests: Mutex::new(Vec::new()),
    };
    let mut account = credentials();
    enrich(&mut account, &client).await.unwrap();
    assert_eq!(
        account.extras[IDENTITY_KEY].as_value(),
        &json!({"issuer":"https://github.com","subject":"42"})
    );
    assert_eq!(account.extras["name"].as_str(), Some("Octocat"));
    assert!(account.extras.get("email").is_none());
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].url, "https://api.github.com/user");
    assert_eq!(
        requests[0].headers["authorization"],
        "Bearer long-account-token"
    );
    drop(requests);
    apply_profile(&mut account, &json!({"id":42,"login":"renamed"}), "").unwrap();
    assert_eq!(account.extras[IDENTITY_KEY].as_value()["subject"], "42");
}

#[test]
fn verified_login_fallback_is_case_insensitive_and_host_specific() {
    let mut account = credentials();
    apply_profile(&mut account, &json!({"login":"Octocat"}), "").unwrap();
    assert_eq!(
        account.extras[IDENTITY_KEY].as_value()["subject"],
        "login:octocat"
    );
    apply_profile(&mut account, &json!({"login":"octocat"}), "acme.ghe.com").unwrap();
    assert_eq!(
        account.extras[IDENTITY_KEY].as_value()["issuer"],
        "https://acme.ghe.com"
    );
    assert_eq!(
        account.extras["accountId"].as_str(),
        Some("octocat@acme.ghe.com")
    );
}

#[tokio::test]
async fn profile_failures_cannot_create_an_unverified_account_identity() {
    for (status, value) in [
        (401, json!({"message":"rejected"})),
        (200, json!({})),
        (200, json!({"login":"bad\nname"})),
    ] {
        let client = Client {
            status,
            value,
            requests: Mutex::new(Vec::new()),
        };
        let mut account = credentials();
        assert!(enrich(&mut account, &client).await.is_err());
        assert!(!has_identity(&account));
    }
}

#[test]
fn arbitrary_enterprise_hosts_and_url_injections_are_rejected() {
    for host in [
        "example.com",
        "https://github.com.evil",
        "https://github.com@evil",
        "http://github.com",
        "a.b.ghe.com",
        "ghe.com",
        "acme.ghe.com:443",
        "acme.ghe.com/path",
    ] {
        assert!(normalize_host(host).is_none(), "{host}");
    }
    assert_eq!(normalize_host("https://GitHub.com/"), Some(String::new()));
    assert_eq!(
        normalize_host("https://Acme.ghe.com/"),
        Some("acme.ghe.com".into())
    );
}
