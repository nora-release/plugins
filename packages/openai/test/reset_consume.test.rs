use super::*;
use async_trait::async_trait;
use nora_ai::{AuthScheme, HttpClientError, HttpStreamingResponse};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

const REQUEST_ID: &str = "af89e508-710e-4ff5-a2e3-7fa640829995";

enum Reply {
    Json(u16, Value),
    Transport,
    Interrupted,
}
struct Client {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<HttpRequest>>,
}
impl Client {
    fn new(replies: impl IntoIterator<Item = Reply>) -> Self {
        Self {
            replies: Mutex::new(replies.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }
    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        match self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP request")
        {
            Reply::Transport => Err(HttpClientError::Transport(
                "private-token private-account upstream-private-details".into(),
            )),
            Reply::Interrupted => Ok(HttpStreamingResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: Box::pin(futures::stream::once(async {
                    Err(HttpClientError::Transport(
                        "private-token private-account truncated reply".into(),
                    ))
                })),
            }),
            Reply::Json(status, value) => {
                let bytes = serde_json::to_vec(&value).unwrap();
                Ok(HttpStreamingResponse {
                    status,
                    headers: BTreeMap::new(),
                    body: Box::pin(futures::stream::once(async move { Ok(bytes.into()) })),
                })
            }
        }
    }
}
fn auth() -> ResolvedProviderAuth {
    ResolvedProviderAuth {
        token: Some("private-token".into()),
        scheme: AuthScheme::Bearer,
        headers: BTreeMap::from([("ChatGPT-Account-ID".into(), "private-account".into())]),
        base_url: None,
        metadata: BTreeMap::new(),
        credential_provider_id: Some("openai-codex".into()),
    }
}
fn credits() -> Value {
    let now = Utc::now();
    json!({"available_count":2,"credits":[
        {"id":"last","status":"available","expires_at":(now+chrono::Duration::days(3)).to_rfc3339()},
        {"id":"first","status":"available","expires_at":(now+chrono::Duration::days(1)).to_rfc3339()}
    ]})
}
fn assert_redacted(error: PluginError) {
    let text = error.to_string();
    for secret in [
        "private-token",
        "private-account",
        "upstream-private-details",
        "truncated reply",
    ] {
        assert!(!text.contains(secret));
    }
}

#[tokio::test]
async fn reset_reads_and_consumes_only_the_selected_chatgpt_account_with_one_request_id() {
    let client = Client::new([
        Reply::Json(200, credits()),
        Reply::Json(200, json!({"code":"reset","windows_reset":2})),
    ]);
    let result = consume(&auth(), &json!(REQUEST_ID.to_uppercase()), &client)
        .await
        .unwrap();
    assert_eq!(result["outcome"], "reset");
    assert_eq!(result["windowsReset"], 2);
    let requests = client.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, HttpMethod::Get);
    assert_eq!(requests[0].url, ENDPOINT);
    assert_eq!(requests[1].method, HttpMethod::Post);
    assert_eq!(requests[1].url, format!("{ENDPOINT}/consume"));
    for request in &requests {
        assert_eq!(request.headers["authorization"], "Bearer private-token");
        assert_eq!(request.headers["chatgpt-account-id"], "private-account");
    }
    let body: Value = serde_json::from_slice(requests[1].body.as_ref().unwrap()).unwrap();
    assert_eq!(
        body,
        json!({"credit_id":"first","redeem_request_id":REQUEST_ID})
    );
    assert!(!result.to_string().contains("private-"));
}

#[tokio::test]
async fn an_invalid_or_missing_request_id_cannot_send_any_http_request() {
    for id in [
        Value::Null,
        json!(""),
        json!("not-a-uuid"),
        json!("00000000-0000-0000-0000-000000000000"),
        json!({"id":REQUEST_ID}),
    ] {
        let client = Client::new([]);
        assert!(consume(&auth(), &id, &client).await.is_err());
        assert!(client.requests().is_empty());
    }
}

#[tokio::test]
async fn api_keys_missing_tokens_and_unknown_account_scopes_cannot_use_credits() {
    let mut cases = Vec::new();
    let mut value = auth();
    value.credential_provider_id = Some("openai-api-key".into());
    cases.push(value);
    let mut value = auth();
    value.credential_provider_id = None;
    cases.push(value);
    let mut value = auth();
    value.token = None;
    cases.push(value);
    let mut value = auth();
    value.headers.clear();
    cases.push(value);
    for account in ["", " ", "private-account\nsecond-header"] {
        let mut value = auth();
        value
            .headers
            .insert("ChatGPT-Account-ID".into(), account.into());
        cases.push(value);
    }
    for value in cases {
        let client = Client::new([]);
        assert!(consume(&value, &json!(REQUEST_ID), &client).await.is_err());
        assert!(client.requests().is_empty());
    }
}

#[tokio::test]
async fn selection_skips_expired_used_and_not_yet_granted_credits() {
    let now = Utc::now();
    let payload = json!({"available_count":8,"credits":[
        {"id":"used","status":"redeemed","expires_at":(now+chrono::Duration::hours(1)).to_rfc3339()},
        {"id":"expired","status":"available","expires_at":(now-chrono::Duration::hours(1)).to_rfc3339()},
        {"id":"future","status":"available","granted_at":(now+chrono::Duration::hours(1)).to_rfc3339(),"expires_at":(now+chrono::Duration::hours(2)).to_rfc3339()},
        {"id":"pending","status":"redeeming","expires_at":(now+chrono::Duration::hours(3)).to_rfc3339()},
        {"id":"already-used","status":"available","redeemed_at":now.to_rfc3339(),"expires_at":(now+chrono::Duration::hours(4)).to_rfc3339()},
        {"id":"valid-later","status":"active","expires_at":(now+chrono::Duration::days(3)).to_rfc3339()},
        {"id":"never-expires","status":"available"},
        {"id":"valid-first","status":"not_redeemed","expires_at":(now+chrono::Duration::days(2)).to_rfc3339()}
    ]});
    let client = Client::new([
        Reply::Json(200, payload),
        Reply::Json(200, json!({"code":"nothing_to_reset"})),
    ]);
    let result = consume(&auth(), &json!(REQUEST_ID), &client).await.unwrap();
    assert_eq!(result["outcome"], "nothing_to_reset");
    let request = client.requests().remove(1);
    let body: Value = serde_json::from_slice(request.body.as_ref().unwrap()).unwrap();
    assert_eq!(body["credit_id"], "valid-first");
}

#[tokio::test]
async fn no_available_credit_never_sends_a_consume_post() {
    for payload in [
        json!({"available_count":0,"credits":[{"id":"stale","status":"available"}]}),
        json!({"credits":[]}),
        json!({"credits":[{"id":"used","status":"redeemed"}]}),
    ] {
        let client = Client::new([Reply::Json(200, payload)]);
        let result = consume(&auth(), &json!(REQUEST_ID), &client).await.unwrap();
        assert_eq!(result["outcome"], "no_credit");
        assert_eq!(client.requests().len(), 1);
        assert_eq!(client.requests()[0].method, HttpMethod::Get);
    }
}

#[tokio::test]
async fn an_ambiguous_post_is_never_retried_and_its_error_contains_no_credentials() {
    for failure in [
        Reply::Transport,
        Reply::Interrupted,
        Reply::Json(
            502,
            json!({"error":"private-token private-account upstream-private-details"}),
        ),
    ] {
        let client = Client::new([Reply::Json(200, credits()), failure]);
        assert_redacted(
            consume(&auth(), &json!(REQUEST_ID), &client)
                .await
                .unwrap_err(),
        );
        let requests = client.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method == HttpMethod::Post)
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn a_failed_or_unrecognized_credit_read_does_not_consume_anything() {
    for response in [
        Reply::Transport,
        Reply::Json(403, json!({"error":"private-token private-account"})),
        Reply::Json(200, json!({"error":{"message":"private-token"}})),
        Reply::Json(200, json!({"unknown":true})),
    ] {
        let client = Client::new([response]);
        assert_redacted(
            consume(&auth(), &json!(REQUEST_ID), &client)
                .await
                .unwrap_err(),
        );
        assert_eq!(client.requests().len(), 1);
    }
}

#[tokio::test]
async fn unsupported_post_outcomes_and_invalid_window_totals_are_not_reported_as_success() {
    for payload in [
        json!({"code":"unknown","message":"private-token"}),
        json!({"code":"reset"}),
        json!({"code":"reset","windows_reset":0}),
        json!({"code":"reset","windows_reset":101}),
    ] {
        let client = Client::new([Reply::Json(200, credits()), Reply::Json(200, payload)]);
        assert_redacted(
            consume(&auth(), &json!(REQUEST_ID), &client)
                .await
                .unwrap_err(),
        );
        assert_eq!(client.requests().len(), 2);
    }
}

#[tokio::test]
async fn recognized_no_spend_outcomes_are_preserved_without_replaying_the_post() {
    for outcome in ["nothing_to_reset", "no_credit", "already_redeemed"] {
        let client = Client::new([
            Reply::Json(200, credits()),
            Reply::Json(200, json!({"code":outcome})),
        ]);
        let result = consume(&auth(), &json!(REQUEST_ID), &client).await.unwrap();
        assert_eq!(result["outcome"], outcome);
        assert_eq!(client.requests().len(), 2);
    }
}
