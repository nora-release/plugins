use super::*;
use provider_support::{HttpByteStream, HttpClientError, HttpStreamingResponse};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;

/// Stands in for GitHub and Copilot: answers each request with the next reply.
pub(crate) struct Client {
    pub requests: StdMutex<Vec<HttpRequest>>,
    replies: StdMutex<VecDeque<(u16, Value)>>,
}

impl Client {
    pub(crate) fn new(replies: Vec<(u16, Value)>) -> Arc<Self> {
        Arc::new(Self {
            requests: StdMutex::default(),
            replies: StdMutex::new(replies.into()),
        })
    }
}

#[async_trait::async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        let (status, value) = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("an unexpected request");
        let bytes = value.to_string().into_bytes();
        let body: HttpByteStream = Box::pin(futures::stream::once(async move { Ok(bytes) }));
        Ok(HttpStreamingResponse {
            status,
            headers: BTreeMap::new(),
            body,
        })
    }
}

fn plugin(replies: Vec<(u16, Value)>) -> (Copilot, Arc<Client>) {
    let client = Client::new(replies);
    let plugin = Copilot {
        client: Some(client.clone()),
        ..Copilot::default()
    };
    (plugin, client)
}

fn session_reply() -> (u16, Value) {
    (
        200,
        json!({"token": "tid=s1", "expires_at": 4_000_000_000u64,
                 "endpoints": {"api": "https://api.individual.githubcopilot.com"}}),
    )
}

#[tokio::test]
async fn a_session_is_traded_once_and_kept() {
    let (plugin, client) = plugin(vec![session_reply()]);
    let first = plugin.session(client.as_ref(), "gho_1").await.unwrap();
    let second = plugin.session(client.as_ref(), "gho_1").await.unwrap();
    assert_eq!(first, second);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, copilot::TOKEN_URL);
    assert_eq!(
        requests[0].headers.get("authorization").map(String::as_str),
        Some("token gho_1")
    );
    assert_eq!(
        requests[0]
            .headers
            .get("editor-version")
            .map(String::as_str),
        Some("vscode/1.140.0")
    );
}

#[tokio::test]
async fn an_account_without_copilot_is_told_so() {
    let (plugin, client) = plugin(vec![(403, json!({"message": "Resource not accessible"}))]);
    let error = plugin.session(client.as_ref(), "gho_1").await.unwrap_err();
    assert!(
        error.to_string().contains("no Copilot subscription"),
        "{error}"
    );
}

#[tokio::test]
async fn a_revoked_github_token_asks_to_sign_in_again() {
    let (plugin, client) = plugin(vec![(401, json!({"message": "Bad credentials"}))]);
    let error: RpcError = plugin
        .session(client.as_ref(), "gho_1")
        .await
        .unwrap_err()
        .into();
    let signed_out: RpcError = github::signed_out().into();
    assert_eq!(error.code, signed_out.code);
}

#[tokio::test]
async fn models_are_read_at_the_sessions_api() {
    let (plugin, client) = plugin(vec![
        session_reply(),
        (
            200,
            json!({"data": [{"id": "gpt-4.1", "model_picker_enabled": true, "capabilities": {"type": "chat"}}]}),
        ),
    ]);
    let question = Question {
        credentials: Some(json!({"github": "gho_1", "login": "octocat"})),
        ..Question::default()
    };
    let models = plugin.models(question).await.unwrap();
    assert_eq!(models[0].id, "gpt-4.1");
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        requests[1].url,
        "https://api.individual.githubcopilot.com/models"
    );
    assert_eq!(
        requests[1].headers.get("authorization").map(String::as_str),
        Some("Bearer tid=s1")
    );
    assert_eq!(
        requests[1]
            .headers
            .get("x-github-api-version")
            .map(String::as_str),
        Some("2026-01-09")
    );
}

#[tokio::test]
async fn signed_out_without_a_github_token() {
    let (plugin, _) = plugin(vec![]);
    assert!(plugin.models(Question::default()).await.is_err());
}

#[tokio::test]
async fn a_new_sign_in_shows_the_code_to_type() {
    let (plugin, _) = plugin(vec![(
        200,
        json!({
            "device_code": "dc", "user_code": "ABCD-1234", "verification_uri": "https://github.com/login/device"
        }),
    )]);
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
    assert_eq!(url, "https://github.com/login/device");
    assert_eq!(code.as_deref(), Some("ABCD-1234"));
}
