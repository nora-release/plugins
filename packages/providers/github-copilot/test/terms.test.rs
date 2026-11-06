use super::*;
use async_trait::async_trait;
use futures::stream;
use nora_ai::{HttpClientError, HttpStreamingResponse};
use nora_provider::ProviderAdapter;
use std::{collections::VecDeque, sync::Mutex as SyncMutex};

struct Client {
    requests: SyncMutex<Vec<HttpRequest>>,
    replies: SyncMutex<VecDeque<(u16, Value)>>,
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        let (status, body) = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected terms request");
        Ok(HttpStreamingResponse {
            status,
            headers: BTreeMap::new(),
            body: Box::pin(stream::once(async move {
                Ok(serde_json::to_vec(&body).unwrap().into())
            })),
        })
    }
}
fn client(replies: Vec<(u16, Value)>) -> Client {
    Client {
        requests: SyncMutex::new(Vec::new()),
        replies: SyncMutex::new(replies.into()),
    }
}
fn outgoing(token: &str, model: &str) -> HttpRequest {
    let mut request = HttpRequest::new(HttpMethod::Post, "https://api.githubcopilot.com/responses")
        .with_header("authorization", format!("Bearer {token}"))
        .with_json_body(&json!({"model":model,"input":"hello"}))
        .unwrap();
    request
        .headers
        .extend(super::super::copilot_client::api_headers());
    request
}
fn waiting(model: &str) -> Value {
    json!({"data":[{"id":model,"model_picker_enabled":true,"capabilities":{"type":"chat"},
        "policy":{"state":"unconfigured", "terms":"https://github.com/settings/copilot/terms"},"supported_endpoints":["/responses"]}]})
}

#[tokio::test]
async fn terms_are_visible_and_accepted_only_when_the_user_calls_that_model() {
    let approvals = Approvals::default();
    let list = waiting("terms-model");
    assert!(super::super::visible(&list["data"][0], "terms-model"));
    let client = client(vec![(200, list), (204, Value::Null)]);
    approvals
        .before_send(&outgoing("account-a", "ordinary-model"), &client)
        .await;
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    approvals
        .before_send(&outgoing("account-a", "terms-model"), &client)
        .await;
    approvals
        .before_send(&outgoing("account-a", "terms-model"), &client)
        .await;
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].url,
        "https://api.githubcopilot.com/models/terms-model/policy"
    );
    assert_eq!(requests[1].headers["authorization"], "Bearer account-a");
    assert_eq!(requests[1].headers["x-github-api-version"], "2026-01-09");
    assert_eq!(
        serde_json::from_slice::<Value>(requests[1].body.as_ref().unwrap()).unwrap(),
        json!({"state":"enabled"})
    );
}

#[tokio::test]
async fn an_approval_for_one_account_does_not_enable_a_different_account() {
    let approvals = Approvals::default();
    let client = client(vec![
        (200, waiting("terms-model")),
        (204, Value::Null),
        (200, waiting("terms-model")),
        (204, Value::Null),
    ]);
    for account in ["first", "second"] {
        approvals
            .before_send(&outgoing(account, "terms-model"), &client)
            .await;
    }
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[1].headers["authorization"], "Bearer first");
    assert_eq!(requests[3].headers["authorization"], "Bearer second");
}

#[tokio::test]
async fn denied_models_without_terms_are_never_enabled_and_lookup_failure_does_not_hide_the_real_error(
) {
    for reply in [
        (
            200,
            json!({"data":[{"id":"denied", "model_picker_enabled":true,
            "capabilities":{"type":"chat"}, "policy":{"state":"disabled"}}]}),
        ),
        (500, json!({"error":"private-account-token"})),
    ] {
        let approvals = Approvals::default();
        let client = client(vec![reply]);
        approvals
            .before_send(&outgoing("account", "denied"), &client)
            .await;
        assert_eq!(client.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn the_hook_is_inert_for_other_adapters_and_api_key_accounts() {
    let client = client(Vec::new());
    let request = outgoing("fixture-key", "model");
    let definition = nora_provider::definition::Definition::from_fields(json!({"id":"fixture","name":"Fixture", "api":"openai-completions", "baseURL":"https://api.githubcopilot.com", "config":{"keyProvider":"fixture"}})).unwrap();
    let credentials = json!({"type":"apiKey", "access":"fixture-key"});
    nora_provider::GenericProvider
        .before_send(&definition, &credentials, &request, &client)
        .await
        .unwrap();
    super::super::Copilot::default()
        .before_send(&definition, &credentials, &request, &client)
        .await
        .unwrap();
    assert!(client.requests.lock().unwrap().is_empty());
}
