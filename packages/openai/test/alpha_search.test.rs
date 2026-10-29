use super::*;
use async_trait::async_trait;
use nora_ai::{AuthScheme, HttpClientError, HttpStreamingResponse};
use std::sync::Mutex;

struct Client {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
    requests: Mutex<Vec<HttpRequest>>,
    transport_error: bool,
}
impl Client {
    fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: BTreeMap::new(),
            body: body.into(),
            requests: Mutex::new(Vec::new()),
            transport_error: false,
        }
    }
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        if self.transport_error {
            return Err(HttpClientError::Transport(
                "private-access private-workspace details".into(),
            ));
        }
        let bytes = self.body.clone();
        Ok(HttpStreamingResponse {
            status: self.status,
            headers: self.headers.clone(),
            body: Box::pin(futures::stream::once(async move { Ok(bytes.into()) })),
        })
    }
}
fn auth() -> ResolvedProviderAuth {
    ResolvedProviderAuth {
        token: Some("private-access".into()),
        scheme: AuthScheme::Bearer,
        headers: BTreeMap::from([("ChatGPT-Account-ID".into(), "bound-workspace".into())]),
        credential_provider_id: Some("openai-codex".into()),
        base_url: Some("https://caller-must-not-choose.test".into()),
        ..Default::default()
    }
}
fn body() -> Value {
    json!({"model":"gpt-fixture","commands":{"search_query":[{"q":"query","domains":["example.test"]}],"open":[{"ref_id":"opaque-ref"}]},"unknown":{"preserved":[null,true,1.5]}})
}
#[tokio::test]
async fn native_search_preserves_opaque_json_and_signs_the_fixed_codex_endpoint_with_bound_credentials(
) {
    let payload = json!({"encrypted_output":"opaque-ciphertext","output":"native result","future":{"keep":[1,true,null]}});
    let client = Client::new(200, serde_json::to_vec(&payload).unwrap());
    let forwarded = BTreeMap::from([
        ("Session-ID".into(), "session-a".into()),
        ("X-Codex-Turn-Metadata".into(), "metadata".into()),
        ("X-OpenAI-Subagent".into(), "guardian".into()),
        ("Version".into(), "99.1.0".into()),
        ("Authorization".into(), "Bearer caller-token".into()),
        ("ChatGPT-Account-ID".into(), "caller-workspace".into()),
        ("Cookie".into(), "private-cookie".into()),
        ("X-Oai-Attestation".into(), "caller-attestation".into()),
        ("X-OpenAI-Codex-Luna-Reserve".into(), "1".into()),
        ("X-Codex-Turn-State".into(), "other-account-state".into()),
        ("X-Codex-Authorization".into(), "nested-secret".into()),
        ("X-OpenAI-Api-Key".into(), "caller-key".into()),
        ("X-Codex-Unsafe".into(), "bad\r\nheader".into()),
        ("X-Other".into(), "unrelated".into()),
    ]);
    let result = search(&auth(), body(), forwarded, &client).await.unwrap();
    assert_eq!(result, json!({"status":200,"body":payload}));
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, HttpMethod::Post);
    assert_eq!(request.url, ENDPOINT);
    assert_eq!(
        serde_json::from_slice::<Value>(request.body.as_ref().unwrap()).unwrap(),
        body()
    );
    assert_eq!(request.headers["authorization"], "Bearer private-access");
    assert_eq!(request.headers["chatgpt-account-id"], "bound-workspace");
    assert_eq!(request.headers["session-id"], "session-a");
    assert_eq!(request.headers["x-codex-turn-metadata"], "metadata");
    assert_eq!(request.headers["x-openai-subagent"], "guardian");
    assert_eq!(request.headers["version"], "99.1.0");
    assert_eq!(request.headers["originator"], "codex_cli_rs");
    assert_eq!(request.headers["openai-beta"], "responses=experimental");
    assert!(request.headers["user-agent"].starts_with("codex_cli_rs/99.1.0 "));
    assert_eq!(request.headers["content-type"], "application/json");
    for name in [
        "cookie",
        "x-oai-attestation",
        "x-openai-codex-luna-reserve",
        "x-codex-turn-state",
        "x-codex-authorization",
        "x-openai-api-key",
        "x-codex-unsafe",
        "x-other",
    ] {
        assert!(!request.headers.contains_key(name), "{name}");
    }
}
#[tokio::test]
async fn vendor_failures_retain_private_status_and_json_for_account_failover() {
    for status in [401, 403, 429, 503] {
        let payload =
            json!({"error":{"type":"rate_limit_error","message":"vendor failure"},"retry_after":5});
        let client = Client::new(status, serde_json::to_vec(&payload).unwrap());
        assert_eq!(
            search(&auth(), body(), BTreeMap::new(), &client)
                .await
                .unwrap(),
            json!({"status":status,"body":payload})
        );
    }
    let client = Client::new(502, b"upstream unavailable".to_vec());
    let result = search(&auth(), body(), BTreeMap::new(), &client)
        .await
        .unwrap();
    assert_eq!(result["status"], 502);
    assert_eq!(result["body"], "upstream unavailable");
}
#[tokio::test]
async fn api_keys_other_providers_and_missing_account_identity_never_send_native_search() {
    let client = Client::new(200, b"{}".to_vec());
    for change in ["api", "other", "token", "account", "invalid-account"] {
        let mut auth = auth();
        match change {
            "api" => auth.credential_provider_id = Some("openai-api-key".into()),
            "other" => auth.credential_provider_id = Some("anthropic".into()),
            "token" => auth.token = None,
            "account" => auth.headers.clear(),
            _ => {
                auth.headers
                    .insert("ChatGPT-Account-ID".into(), "invalid\naccount".into());
            }
        }
        assert!(search(&auth, body(), BTreeMap::new(), &client)
            .await
            .is_err());
    }
    assert!(client.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn invalid_or_oversized_replies_and_transport_failures_are_bounded_and_redacted() {
    for bytes in [
        b"not JSON".to_vec(),
        b"[]".to_vec(),
        vec![b' '; OUTPUT_LIMIT + 1],
        b"{\"output\":\"\xff\"}".to_vec(),
    ] {
        let client = Client::new(200, bytes);
        let error = search(&auth(), body(), BTreeMap::new(), &client)
            .await
            .unwrap_err();
        assert_eq!(error.code, "upstream_transport_error");
        assert!(!error.message.contains("private-access"));
    }
    let mut client = Client::new(200, b"{}".to_vec());
    client.transport_error = true;
    let error = search(&auth(), body(), BTreeMap::new(), &client)
        .await
        .unwrap_err();
    assert!(
        !error.message.contains("private-access") && !error.message.contains("private-workspace")
    );
}
#[tokio::test]
async fn invalid_and_oversized_request_objects_are_rejected_before_network_access() {
    let client = Client::new(200, b"{}".to_vec());
    for request in [
        Value::Null,
        json!([]),
        json!({"oversized":"a".repeat(MAX_REQUEST_BYTES)}),
    ] {
        assert_eq!(
            search(&auth(), request, BTreeMap::new(), &client)
                .await
                .unwrap_err()
                .code,
            "invalid_request"
        );
    }
    assert!(client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn signed_client_version_is_never_downgraded_and_long_vendor_errors_keep_their_status() {
    let client = Client::new(200, b"{}".to_vec());
    let mut auth = auth();
    auth.headers.insert("version".into(), "0.200.0".into());
    search(
        &auth,
        body(),
        BTreeMap::from([("version".into(), "0.100.0".into())]),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(
        client.requests.lock().unwrap()[0].headers["version"],
        "0.200.0"
    );
    let client = Client::new(429, vec![b'x'; 8192]);
    let result = search(&auth, body(), BTreeMap::new(), &client)
        .await
        .unwrap();
    assert_eq!(result["status"], 429);
    assert_eq!(result["body"].as_str().unwrap().len(), 4096);
}

#[tokio::test]
async fn retry_headers_are_private_bounded_and_use_the_shared_failure_parser() {
    for (name, value, expected) in [
        ("Retry-After", "12", 12000),
        ("retry-after-ms", "250", 250),
        ("Retry-After", "999999", 120000),
    ] {
        let mut client = Client::new(429, b"{}".to_vec());
        client.headers.insert(name.into(), value.into());
        let result = search(&auth(), body(), BTreeMap::new(), &client)
            .await
            .unwrap();
        assert_eq!(result["retryAfterMs"], expected);
        assert_eq!(result["body"], json!({}));
    }
    let mut client = Client::new(429, b"{}".to_vec());
    client
        .headers
        .insert("Retry-After".into(), "Wed, 21 Oct 2037 07:28:00 GMT".into());
    let result = search(&auth(), body(), BTreeMap::new(), &client)
        .await
        .unwrap();
    assert_eq!(result["retryAfter"], "Wed, 21 Oct 2037 07:28:00 GMT");
    assert!(result.get("retryAfterMs").is_none());
}
