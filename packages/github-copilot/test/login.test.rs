use super::*;
use nora_ai::{HttpClientError, HttpRequest, HttpResponse, HttpStreamingResponse};
use serde_json::json;
use std::{collections::VecDeque, sync::Mutex};

struct Client {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<Value>>,
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, _: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        unreachable!("Sign-in uses the mocked one-shot client")
    }
    async fn request(&self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected sign-in request");
        Ok(HttpResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: serde_json::to_vec(&reply).unwrap(),
        })
    }
}

#[tokio::test]
async fn enterprise_device_sign_in_keeps_the_host_on_identity_models_and_usage() {
    let client = Client {
        requests: Mutex::new(Vec::new()),
        replies: Mutex::new(VecDeque::from([
            json!({"user_code":"TEST", "device_code":"fixture-device", "verification_uri":"https://acme.ghe.com/login/device", "interval":0}),
            json!({"access_token":"fixture-github-token"}),
            json!({"token":"fixture-session", "expires_at":nora_ai::oauth::now_millis()/1000+3600}),
            json!({"id":42,"login":"person"}),
        ])),
    };
    let provider = Copilot::default();
    let credentials = provider
        .login_with_inputs(
            OAuthLoginCallbacks::silent(),
            json!({"host":"https://Acme.ghe.com/"}),
            &client,
        )
        .await
        .unwrap();
    assert_eq!(
        credentials.extras["githubHost"].as_str(),
        Some("acme.ghe.com")
    );
    assert_eq!(
        credentials.extras["accountIdentity"].as_value()["issuer"],
        "https://acme.ghe.com"
    );
    assert_eq!(
        credentials.extras["accountIdentity"].as_value()["subject"],
        "42"
    );
    assert_eq!(
        credentials.extras["accountId"].as_str(),
        Some("person@acme.ghe.com")
    );
    let mut auth = nora_provider::auth::base_auth("github-copilot", &credentials);
    provider.enrich_auth(&credentials, &mut auth);
    assert_eq!(
        auth.base_url.as_deref(),
        Some("https://copilot-api.acme.ghe.com")
    );
    assert_eq!(auth.metadata["githubHost"].as_str(), Some("acme.ghe.com"));
    let encoded = nora_provider::auth::encode(&credentials, "github-copilot", false);
    assert_eq!(
        provider.endpoint(Some(&encoded)).as_deref(),
        Some("https://copilot-api.acme.ghe.com")
    );
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .map(|request| request.url.as_str())
            .collect::<Vec<_>>(),
        [
            "https://acme.ghe.com/login/device/code",
            "https://acme.ghe.com/login/oauth/access_token",
            "https://api.acme.ghe.com/copilot_internal/v2/token",
            "https://api.acme.ghe.com/user",
        ]
    );
}

#[tokio::test]
async fn enterprise_sign_in_rejects_untrusted_hosts_before_requesting_a_device_code() {
    let client = Client {
        requests: Mutex::new(Vec::new()),
        replies: Mutex::new(VecDeque::new()),
    };
    for host in [
        json!("evil.invalid"),
        json!("github.com@evil.invalid"),
        json!("https://acme.ghe.com/private"),
        json!(false),
    ] {
        assert!(Copilot::default()
            .login_with_inputs(OAuthLoginCallbacks::silent(), json!({"host":host}), &client)
            .await
            .is_err());
    }
    assert!(client.requests.lock().unwrap().is_empty());
}
