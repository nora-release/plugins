use super::*;
use provider_support::{HttpByteStream, HttpClientError, HttpStreamingResponse};
use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;

/// Stands in for ChatGPT: answers each request with the next reply.
struct Client {
    requests: StdMutex<Vec<HttpRequest>>,
    replies: StdMutex<VecDeque<(u16, Value)>>,
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

fn plugin(replies: Vec<(u16, Value)>) -> (ChatGpt, Arc<Client>) {
    let client = Arc::new(Client {
        requests: StdMutex::default(),
        replies: StdMutex::new(replies.into()),
    });
    let plugin = ChatGpt {
        client: Some(client.clone()),
        prompts_file: None,
        ..ChatGpt::default()
    };
    (plugin, client)
}

fn saved() -> Value {
    json!({"access": "a1", "refresh": "r1", "accountID": "acct-1", "email": "ada@example.test"})
}

#[tokio::test]
async fn a_refresh_token_is_spent_once_and_its_answer_shared() {
    let access = crate::auth::tests::jwt(json!({"exp": 1_900_000_000u64}));
    let (plugin, client) = plugin(vec![(
        200,
        json!({"access_token": access, "refresh_token": "r2"}),
    )]);
    let first = plugin.renew(client.as_ref(), &saved()).await.unwrap();
    let second = plugin.renew(client.as_ref(), &saved()).await.unwrap();
    assert_eq!(first, second);
    assert_eq!(auth::text(&first, "refresh"), "r2");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, auth::TOKEN_URL);
    assert_eq!(
        requests[0].headers.get("originator").map(String::as_str),
        Some("codex_cli_rs")
    );
}

#[tokio::test]
async fn the_model_list_is_asked_as_codex_and_keeps_the_instructions() {
    let (plugin, client) = plugin(vec![(
        200,
        json!({"models": [{"slug": "gpt-5.5", "base_instructions": "official"}]}),
    )]);
    let models = plugin.list(client.as_ref(), &saved()).await.unwrap();
    assert_eq!(models[0].id, "gpt-5.5");
    assert_eq!(
        plugin
            .instructions(client.as_ref(), &saved(), "gpt-5.5")
            .await
            .as_deref(),
        Some("official")
    );
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "the kept instructions are not asked again"
    );
    let headers = &requests[0].headers;
    assert!(requests[0]
        .url
        .starts_with("https://chatgpt.com/backend-api/codex/models?client_version="));
    assert_eq!(
        headers.get("authorization").map(String::as_str),
        Some("Bearer a1")
    );
    assert_eq!(
        headers.get("chatgpt-account-id").map(String::as_str),
        Some("acct-1")
    );
}

#[tokio::test]
async fn a_turned_away_sign_in_reads_as_signing_in_again() {
    let (plugin, client) = plugin(vec![(401, json!({"detail": "expired"}))]);
    let error = plugin.list(client.as_ref(), &saved()).await.unwrap_err();
    assert_eq!(
        RpcError::from(error).code,
        nora_plugin_runtime_sdk::provider::SIGN_IN_NEEDED
    );
}

/// A sign-in started again takes over the port the one before held.
#[tokio::test]
async fn a_sign_in_started_again_takes_the_port_over() {
    let (plugin, _) = plugin(vec![]);
    let request = || SignIn {
        question: Question {
            provider: "chatgpt".into(),
            ..Question::default()
        },
        way: "browser".into(),
        inputs: BTreeMap::new(),
    };
    let Ok(Step::Open { session: first, .. }) = plugin.sign_in(request()).await else {
        return; // port 1455 is another app's on this machine
    };
    let Step::Open {
        session: second, ..
    } = plugin.sign_in(request()).await.unwrap()
    else {
        panic!("no page to open");
    };
    assert_ne!(first, second);
    let finish = |session: String| SignInFinish {
        question: Question {
            provider: "chatgpt".into(),
            ..Question::default()
        },
        session,
        code: String::new(),
    };
    assert!(
        plugin.sign_in_finish(finish(first)).await.is_err(),
        "the sign-in taken over still waits"
    );
    assert_eq!(
        plugin.sign_in_finish(finish(second)).await.unwrap(),
        Step::Waiting
    );
}
