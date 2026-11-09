use super::*;
use provider_support::{HttpByteStream, HttpClientError};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;

/// Stands in for xAI and Grok: answers each request with the next reply.
struct Client {
    requests: StdMutex<Vec<HttpRequest>>,
    replies: StdMutex<VecDeque<(u16, String)>>,
}

#[async_trait::async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        let (status, text) = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("an unexpected request");
        let bytes = text.into_bytes();
        let body: HttpByteStream = Box::pin(futures::stream::once(async move { Ok(bytes) }));
        Ok(HttpStreamingResponse {
            status,
            headers: BTreeMap::new(),
            body,
        })
    }
}

fn plugin(replies: Vec<(u16, String)>) -> (Grok, Arc<Client>) {
    let client = Arc::new(Client {
        requests: StdMutex::default(),
        replies: StdMutex::new(replies.into()),
    });
    let plugin = Grok {
        client: Some(client.clone()),
        ..Grok::default()
    };
    (plugin, client)
}

fn signed() -> Question {
    Question {
        credentials: Some(json!({"access": "a1", "refresh": "r1", "expires": 4_000_000_000u64})),
        ..Question::default()
    }
}

#[tokio::test]
async fn the_version_is_grok_builds_stable_one_read_once() {
    let (plugin, client) = plugin(vec![(200, "1.0.80\n".into())]);
    assert_eq!(plugin.version(client.as_ref()).await, "1.0.80");
    assert_eq!(plugin.version(client.as_ref()).await, "1.0.80");
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn the_version_falls_back_when_the_channel_cant_be_read() {
    let (plugin, client) = plugin(vec![(404, String::new()), (200, "<html>".into())]);
    assert_eq!(plugin.version(client.as_ref()).await, grok::VERSION);
}

#[tokio::test]
async fn a_refresh_token_is_spent_once_and_its_answer_shared() {
    let (plugin, client) = plugin(vec![
        (200, "1.0.77".into()),
        (
            200,
            json!({"access_token": "a2", "refresh_token": "r2", "expires_in": 3600}).to_string(),
        ),
    ]);
    let saved = json!({"access": "a1", "refresh": "r1"});
    let first = plugin.renew(client.as_ref(), &saved).await.unwrap();
    let second = plugin.renew(client.as_ref(), &saved).await.unwrap();
    assert_eq!(first, second);
    assert_eq!(first["refresh"], "r2");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].url, xai::TOKEN_URL);
    let body = String::from_utf8(requests[1].body.clone().unwrap()).unwrap();
    assert!(body.contains("grant_type=refresh_token") && body.contains("refresh_token=r1"));
}

#[tokio::test]
async fn a_refused_renewal_asks_to_sign_in_again() {
    let (plugin, client) = plugin(vec![
        (200, "1.0.77".into()),
        (
            400,
            json!({"error": "invalid_grant", "error_description": "refresh token revoked"})
                .to_string(),
        ),
    ]);
    let error: RpcError = plugin
        .renew(client.as_ref(), &json!({"access": "a1", "refresh": "r1"}))
        .await
        .unwrap_err()
        .into();
    let signed_out: RpcError = xai::signed_out().into();
    assert_eq!(error.code, signed_out.code);
    assert!(
        error.message.contains("refresh token revoked"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn models_are_read_signed() {
    let (plugin, client) = plugin(vec![
        (200, "1.0.77".into()),
        (
            200,
            json!({"data": [{"id": "grok-4.7", "context_window": 256000}]}).to_string(),
        ),
    ]);
    let models = plugin.models(signed()).await.unwrap();
    assert_eq!(models[0].id, "grok-4.7");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[1].url, format!("{}/models", grok::BASE));
    assert_eq!(
        requests[1].headers.get("authorization").map(String::as_str),
        Some("Bearer a1")
    );
}

#[tokio::test]
async fn a_sign_in_shows_the_code_on_xais_page() {
    let (plugin, _) = plugin(vec![
        (200, "1.0.77".into()),
        (200, json!({
            "device_code": "dc", "user_code": "4FVQ-6RPF",
            "verification_uri": "https://accounts.x.ai/oauth2/device",
            "verification_uri_complete": "https://accounts.x.ai/oauth2/device?user_code=4FVQ-6RPF",
            "interval": 5, "expires_in": 1800
        }).to_string()),
    ]);
    let step = plugin
        .sign_in(SignIn {
            question: Question::default(),
            way: "browser".into(),
            inputs: BTreeMap::new(),
        })
        .await
        .unwrap();
    let Step::Open { url, code, .. } = step else {
        panic!("a page to open")
    };
    assert_eq!(
        url,
        "https://accounts.x.ai/oauth2/device?user_code=4FVQ-6RPF"
    );
    assert_eq!(code.as_deref(), Some("4FVQ-6RPF"));
}

#[test]
fn a_request_names_the_plain_model_and_its_window() {
    let outgoing = Outgoing::of(
        json!({"model": "grok-4.7@500k", "input": "hi", "prompt_cache_key": "conv-1"})
            .to_string()
            .as_bytes(),
    )
    .unwrap();
    let body: Value = serde_json::from_slice(&outgoing.body).unwrap();
    assert_eq!(body["model"], "grok-4.7");
    let header = |name: &str| {
        outgoing
            .headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(header("x-grok-model-override"), Some("grok-4.7"));
    assert_eq!(header("x-grok-context-window"), Some("500000"));
    assert_eq!(header("x-grok-conv-id"), Some("conv-1"));
    assert_eq!(header("x-grok-doom-loop-check"), Some("1024"));
}
