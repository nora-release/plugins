use super::*;
use async_trait::async_trait;
use futures::stream;
use nora_ai::{HttpClientError, HttpMethod};
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicU64, Ordering},
};

const REFUSAL: &str = r#"{"error":"x-api-key and Authorization cannot both be present"}"#;

fn definition() -> Definition {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = format!("relay-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let mut definition = Definition::from_fields(json!({"id":id,"name":"Relay","api":"anthropic-messages","baseURL":"https://relay.invalid/v1","config":{"keyProvider":"relay"}})).unwrap();
    definition.request_account = Some("one".into());
    definition
}
fn credentials() -> Value {
    json!({"type":"apiKey","access":"private-token"})
}
fn request() -> HttpRequest {
    HttpRequest::new(HttpMethod::Post, "https://relay.invalid/v1/messages")
        .with_header("x-api-key", "private-token")
        .with_header("Authorization", "Bearer private-token")
        .with_body(b"{\"model\":\"actual-model\",\"input\":\"hello\"}".to_vec())
}
struct Client {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<(u16, Vec<Vec<u8>>)>>,
}
impl Client {
    fn new(replies: Vec<(u16, &str)>) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            replies: Mutex::new(
                replies
                    .into_iter()
                    .map(|(status, body)| (status, vec![body.as_bytes().to_vec()]))
                    .collect(),
            ),
        }
    }
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        let (status, chunks) = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected auth retry");
        Ok(HttpStreamingResponse {
            status,
            headers: BTreeMap::from([("x-original".into(), "kept".into())]),
            body: Box::pin(stream::iter(
                chunks.into_iter().map(|chunk| Ok(chunk.into())),
            )),
        })
    }
}
fn header<'a>(request: &'a HttpRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[test]
fn only_the_exact_bilingual_dual_header_refusal_is_recognized() {
    for body in [
        REFUSAL,
        "x-api-key 和 Authorization 不可以同时存在",
        "Only one of x-api-key or Authorization header may be provided",
    ] {
        assert!(both_refused(401, body.as_bytes()));
        for status in [400, 403, 429, 500] {
            assert!(!both_refused(status, body.as_bytes()));
        }
    }
    for body in [
        "invalid api key",
        "invalid x-api-key",
        "Authorization failed",
        "both credentials invalid",
    ] {
        assert!(!both_refused(401, body.as_bytes()));
    }
}

#[tokio::test]
async fn a_refusal_replays_the_same_body_once_with_each_header_and_keeps_the_success() {
    let definition = definition();
    let client = Client::new(vec![
        (401, REFUSAL),
        (403, "no"),
        (200, "ok"),
        (200, "next"),
    ]);
    let original = request();
    let response = send(
        &client,
        &definition,
        &credentials(),
        "anthropic-messages",
        original.clone(),
    )
    .await
    .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(
        choice(&definition, &credentials(), &original.url),
        Some("bearer")
    );
    send(
        &client,
        &definition,
        &credentials(),
        "anthropic-messages",
        original.clone(),
    )
    .await
    .unwrap();
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(
        header(&requests[0], "x-api-key").is_some()
            && header(&requests[0], "authorization").is_some()
    );
    assert_eq!(header(&requests[1], "authorization"), None);
    assert_eq!(header(&requests[2], "x-api-key"), None);
    assert_eq!(header(&requests[3], "x-api-key"), None);
    for sent in requests.iter() {
        assert_eq!(sent.body, original.body);
        assert_eq!(sent.url, original.url);
    }
}

#[tokio::test]
async fn ordinary_401_and_successful_streams_are_never_retried() {
    for (status, body) in [
        (401, "invalid key"),
        (403, REFUSAL),
        (200, REFUSAL),
        (429, REFUSAL),
    ] {
        let definition = definition();
        let client = Client::new(vec![(status, body)]);
        let response = send(
            &client,
            &definition,
            &credentials(),
            "anthropic-messages",
            request(),
        )
        .await
        .unwrap();
        assert_eq!(response.status, status);
        assert_eq!(
            nora_ai::collect_http_body_text_limited(response.body, 1024 * 1024)
                .await
                .unwrap(),
            body
        );
        assert_eq!(client.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn copilot_refusals_are_observed_without_changing_the_original_error_bytes_or_headers() {
    let mut definition = definition();
    definition.adapter = crate::test_adapter("github-copilot");
    let credentials = json!({"type":"oauth", "access":"token", "extras":{"accountIdentity":{"issuer":"https://github.com","subject":"one"}}});
    let mut bytes = br#"{"error":{"code":"model_not_supported"}}"#.to_vec();
    bytes.extend(vec![b' '; 100_000]);
    let client = Client {
        requests: Mutex::new(Vec::new()),
        replies: Mutex::new(VecDeque::from([
            (200, vec![br#"{"data":[]}"#.to_vec()]),
            (
                400,
                vec![
                    bytes[..17].to_vec(),
                    bytes[17..70_000].to_vec(),
                    bytes[70_000..].to_vec(),
                ],
            ),
        ])),
    };
    let response = crate::transport::send(
        &client,
        &definition,
        &credentials,
        "openai-responses",
        request(),
    )
    .await
    .unwrap();
    assert_eq!(response.status, 400);
    assert_eq!(response.headers["x-original"], "kept");
    let body = nora_ai::collect_http_body_text_limited(response.body, 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(body.as_bytes(), bytes);
    let row =
        json!({"id":"actual-model", "model_picker_enabled":true,"capabilities":{"type":"chat"}});
    assert!(!definition
        .adapter
        .model_visible(&row, "actual-model", &credentials));
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn adapters_without_an_error_observer_leave_the_response_stream_unread() {
    struct Lazy(std::sync::Arc<std::sync::atomic::AtomicU64>);
    #[async_trait]
    impl HttpClient for Lazy {
        async fn stream(&self, _: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
            let reads = self.0.clone();
            Ok(HttpStreamingResponse {
                status: 400,
                headers: BTreeMap::new(),
                body: Box::pin(stream::once(async move {
                    reads.fetch_add(1, Ordering::SeqCst);
                    Ok(b"original-error".to_vec().into())
                })),
            })
        }
    }
    for api_account in [false, true] {
        let mut definition = definition();
        let credentials = if api_account {
            definition.adapter = crate::test_adapter("github-copilot");
            json!({"type":"apiKey","access":"token"})
        } else {
            json!({"type":"oauth","access":"token"})
        };
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let response = crate::transport::send(
            &Lazy(reads.clone()),
            &definition,
            &credentials,
            "openai-responses",
            request(),
        )
        .await
        .unwrap();
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        assert_eq!(
            nora_ai::collect_http_body_text_limited(response.body, 1024)
                .await
                .unwrap(),
            "original-error"
        );
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn explicit_choices_custom_auth_headers_and_other_protocols_cannot_be_changed() {
    for kind in ["choice", "custom-header", "oauth", "official", "chat"] {
        let mut definition = definition();
        let mut credentials = credentials();
        let mut request = request();
        let api = if kind == "chat" {
            "openai-completions"
        } else {
            "anthropic-messages"
        };
        match kind {
            "choice" => credentials["extras"] = json!({"authHeader":"x-api-key"}),
            "custom-header" => {
                definition
                    .config
                    .headers
                    .insert("Authorization".into(), "Bearer private-token".into());
            }
            "oauth" => credentials["type"] = json!("oauth"),
            "official" => request.url = "https://api.anthropic.com/v1/messages".into(),
            _ => {}
        }
        let client = Client::new(vec![(401, REFUSAL)]);
        assert_eq!(
            send(&client, &definition, &credentials, api, request)
                .await
                .unwrap()
                .status,
            401
        );
        assert_eq!(client.requests.lock().unwrap().len(), 1, "{kind}");
    }
}

#[tokio::test]
async fn learned_headers_are_scoped_to_the_provider_account_origin_and_key() {
    let definition = definition();
    remember(
        &definition,
        &credentials(),
        "https://relay.invalid/v1/messages",
        "x-api-key",
    )
    .await;
    assert_eq!(
        choice(
            &definition,
            &credentials(),
            "https://relay.invalid/other/path"
        ),
        Some("x-api-key")
    );
    assert_eq!(
        choice(
            &definition,
            &credentials(),
            "https://other.invalid/v1/messages"
        ),
        None
    );
    let mut other = definition.clone();
    other.request_account = Some("two".into());
    assert_eq!(
        choice(&other, &credentials(), "https://relay.invalid/v1/messages"),
        None
    );
    let mut other = definition.clone();
    other.id.push_str("-other");
    assert_eq!(
        choice(&other, &credentials(), "https://relay.invalid/v1/messages"),
        None
    );
    assert_eq!(
        choice(
            &definition,
            &json!({"type":"apiKey","access":"changed"}),
            "https://relay.invalid/v1/messages"
        ),
        None
    );
    let saved = json!({"type":"apiKey","access":"private-token","extras":{"authHeader":"bearer","noraLearnedAuthHeaders":{"https://relay.invalid":"x-api-key"}}});
    assert_eq!(
        choice(&definition, &saved, "https://relay.invalid/v1/messages"),
        Some("bearer")
    );
}

#[tokio::test]
async fn refusal_inspection_preserves_every_original_error_byte_without_a_match() {
    let definition = definition();
    let client = Client::new(vec![]);
    let chunks = vec![
        b"invalid key ".to_vec(),
        vec![b'x'; 70 * 1024],
        b" final bytes".to_vec(),
    ];
    client
        .replies
        .lock()
        .unwrap()
        .push_back((401, chunks.clone()));
    let response = send(
        &client,
        &definition,
        &credentials(),
        "anthropic-messages",
        request(),
    )
    .await
    .unwrap();
    assert_eq!(response.headers["x-original"], "kept");
    let body = nora_ai::collect_http_body_text_limited(response.body, 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(body.as_bytes(), chunks.concat());
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn all_single_header_failures_stop_after_three_attempts_without_caching() {
    let definition = definition();
    let client = Client::new(vec![(401, REFUSAL), (401, "first"), (403, "last")]);
    let response = send(
        &client,
        &definition,
        &credentials(),
        "anthropic-messages",
        request(),
    )
    .await
    .unwrap();
    assert_eq!(response.status, 403);
    assert_eq!(
        nora_ai::collect_http_body_text_limited(response.body, 1024 * 1024)
            .await
            .unwrap(),
        "last"
    );
    assert_eq!(
        choice(
            &definition,
            &credentials(),
            "https://relay.invalid/v1/messages"
        ),
        None
    );
    assert_eq!(client.requests.lock().unwrap().len(), 3);
}
