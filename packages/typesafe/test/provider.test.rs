use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use nora_ai::{HttpClientError, HttpMethod, HttpStreamingResponse};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};

struct Client {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<HttpStreamingResponse>>,
}
impl Client {
    fn new(values: Vec<Value>) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            replies: Mutex::new(
                values
                    .into_iter()
                    .map(|value| {
                        Self::response(200, BTreeMap::new(), serde_json::to_vec(&value).unwrap())
                    })
                    .collect(),
            ),
        }
    }
    fn response(
        status: u16,
        headers: BTreeMap<String, String>,
        body: Vec<u8>,
    ) -> HttpStreamingResponse {
        HttpStreamingResponse {
            status,
            headers,
            body: Box::pin(futures::stream::once(async move { Ok(body.into()) })),
        }
    }
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| HttpClientError::Transport("Unexpected network request".into()))
    }
}
fn definition() -> Definition {
    Definition::parse(serde_json::from_str(include_str!("../nora-provider.json")).unwrap()).unwrap()
}
fn credentials() -> Value {
    json!({"type":"apiKey","access":"private-typesafe-key"})
}
fn context() -> CallContext {
    CallContext {
        account: "one".into(),
        credentials: Some(credentials()),
        network_proxy: None,
    }
}
fn question() -> Value {
    json!({"model":"custom-decider","state":{"message":"Write tests","previous_message_kind":"code"},"questions":{"intent":{"type":"choice","instructions":"Which kind?","criteria":{"code":null,"none of these":"Other"}},"effort":{"type":"score","instructions":"How hard?","criteria":["low","medium","high","xhigh"]},"levels":{"type":"noul","instructions":"Are these levels?"}}})
}
fn request(body: Value) -> FetchRequest {
    FetchRequest {
        account: "one".into(),
        credentials: Some(credentials()),
        network_proxy: None,
        url: "https://api.typesafe.ai/v1/systemone".into(),
        method: "POST".into(),
        headers: BTreeMap::from([
            ("content-type".into(), "application/json".into()),
            ("authorization".into(), "Bearer nora-plugin".into()),
        ]),
        metadata: BTreeMap::from([("sessionID".into(), "private-session-id".into())]),
        body: Some(STANDARD.encode(serde_json::to_vec(&body).unwrap())),
        body_file: None,
    }
}
fn answer() -> Value {
    json!({"model":"custom-decider","answers":{"intent":{"choice":"code","confidence":0.75,"probabilities":{"code":0.9,"none of these":0.1}},"effort":{"score":2.1}},"usage":{"input_tokens":23,"output_tokens":4}})
}
async fn response_json(mut response: FetchResponse) -> Value {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.body.next().await {
        bytes.extend(chunk.unwrap());
    }
    serde_json::from_slice(&bytes).unwrap()
}

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "nora-typesafe-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn manifest_declares_only_decisions_and_api_key_authentication() {
    let manifest: Value = serde_json::from_str(include_str!("../nora-provider.json")).unwrap();
    assert_eq!(manifest["api"], "systemone");
    assert_eq!(manifest["config"]["operations"], json!(["decision"]));
    assert_eq!(manifest["models"][0]["info"]["decision"], true);
    assert_eq!(manifest["models"][0]["info"]["chat"], false);
    assert_eq!(manifest["authMethods"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn model_discovery_uses_real_authenticated_http_and_explicit_capabilities() {
    let client = Client::new(vec![
        json!({"data":[{"id":"a-completely-custom-name","displayName":"Decision A","context_length":65536,"modalities":{"input":["text","image"]}},{"id":"a-completely-custom-name"},{"id":"not-named-jev"}]}),
    ]);
    let result = list_models(&definition(), context(), &client)
        .await
        .unwrap();
    assert_eq!(result["source"], "remote");
    assert_eq!(result["allowUnknownModels"], false);
    assert_eq!(result["models"].as_array().unwrap().len(), 2);
    assert_eq!(result["models"][0]["id"], "a-completely-custom-name");
    assert_eq!(result["models"][0]["contextWindow"], 65536);
    for model in result["models"].as_array().unwrap() {
        assert_eq!(model["api"], "systemone");
        assert_eq!(model["info"]["chat"], false);
        assert_eq!(model["info"]["decision"], true);
    }
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, HttpMethod::Get);
    assert_eq!(requests[0].url, "https://api.typesafe.ai/v1/models");
    assert_eq!(
        requests[0].headers["Authorization"],
        "Bearer private-typesafe-key"
    );
}

#[tokio::test]
async fn an_empty_remote_list_is_authoritative_and_failures_are_not_called_remote_data() {
    let client = Client::new(vec![json!({"data":[]})]);
    assert!(list_models(&definition(), context(), &client)
        .await
        .unwrap()["models"]
        .as_array()
        .unwrap()
        .is_empty());
    let client = Client {
        requests: Mutex::default(),
        replies: Mutex::new(VecDeque::from([Client::response(
            401,
            BTreeMap::new(),
            b"private-typesafe-key refused".to_vec(),
        )])),
    };
    let error = list_models(&definition(), context(), &client)
        .await
        .unwrap_err();
    assert!(!error.message.contains("private-typesafe-key"));
    assert!(error.message.contains("401"));
    let client = Client::new(vec![]);
    let preset = list_models(&definition(), CallContext::default(), &client)
        .await
        .unwrap();
    assert_eq!(preset["source"], "preset");
    assert_eq!(preset["models"][0]["info"]["catalogSource"], "preset");
    assert!(client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn systemone_preserves_questions_answers_probabilities_and_usage() {
    let client = Client::new(vec![answer()]);
    let response = fetch(&definition(), request(question()), &client)
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response_json(response).await, answer());
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].url, "https://api.typesafe.ai/v1/systemone");
    assert_eq!(requests[0].method, HttpMethod::Post);
    assert_eq!(
        serde_json::from_slice::<Value>(requests[0].body.as_ref().unwrap()).unwrap(),
        question()
    );
    assert_eq!(
        requests[0].headers["Authorization"],
        "Bearer private-typesafe-key"
    );
    assert!(!format!("{:?}", requests[0]).contains("private-session-id"));
    assert!(!format!("{:?}", requests[0]).contains("nora-plugin"));
}

#[tokio::test]
async fn chat_foreign_origins_queries_and_non_post_requests_are_rejected_before_network() {
    let client = Client::new(vec![]);
    for target in [
        "https://foreign.example/v1/systemone",
        "https://api.typesafe.ai/v1/chat/completions",
        "https://api.typesafe.ai/v1/responses",
        "https://api.typesafe.ai/v1/systemone?key=foreign",
        "https://user:pass@api.typesafe.ai/v1/systemone",
        "https://api.typesafe.ai/v1/systemone/../models",
    ] {
        let mut request = request(question());
        request.url = target.into();
        assert!(fetch(&definition(), request, &client).await.is_err());
    }
    let mut non_post = request(question());
    non_post.method = "GET".into();
    assert!(fetch(&definition(), non_post, &client).await.is_err());
    assert!(client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn custom_endpoints_preserve_their_path_prefix_and_never_use_the_default_host() {
    let mut definition = definition();
    definition.base_url = "https://decisions.example/team/v2".into();
    let client = Client::new(vec![answer()]);
    let mut call = request(question());
    call.url = "https://decisions.example/team/v2/systemone".into();
    fetch(&definition, call, &client).await.unwrap();
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        requests[0].url,
        "https://decisions.example/team/v2/systemone"
    );
    assert_eq!(
        requests[0].headers["Authorization"],
        "Bearer private-typesafe-key"
    );
}

#[tokio::test]
async fn errors_retain_status_and_retry_after_without_revealing_the_key() {
    let client = Client {
        requests: Mutex::default(),
        replies: Mutex::new(VecDeque::from([Client::response(
            429,
            BTreeMap::from([
                ("retry-after".into(), "7".into()),
                ("set-cookie".into(), "secret-cookie".into()),
                ("content-length".into(), "100".into()),
            ]),
            br#"{"error":{"message":"private-typesafe-key exhausted"}}"#.to_vec(),
        )])),
    };
    let response = fetch(&definition(), request(question()), &client)
        .await
        .unwrap();
    assert_eq!(response.status, 429);
    assert_eq!(response.headers["retry-after"], "7");
    assert!(!response.headers.contains_key("set-cookie"));
    assert!(!response.headers.contains_key("content-length"));
    assert!(!response_json(response)
        .await
        .to_string()
        .contains("private-typesafe-key"));
}

#[tokio::test]
async fn malformed_or_oversized_answers_fail_explicitly() {
    let client = Client::new(vec![
        json!({"choices":[{"message":{"content":"chat answer"}}]}),
    ]);
    assert!(fetch(&definition(), request(question()), &client)
        .await
        .is_err());
    let client = Client {
        requests: Mutex::default(),
        replies: Mutex::new(VecDeque::from([Client::response(
            200,
            BTreeMap::new(),
            vec![b' '; MAX_RESPONSE + 1],
        )])),
    };
    assert!(fetch(&definition(), request(question()), &client)
        .await
        .err()
        .unwrap()
        .message
        .contains("1 MiB"));
}

#[test]
fn invalid_questions_and_private_body_sources_never_become_upstream_requests() {
    for value in [
        json!({"model":"m","state":{},"questions":{}}),
        json!({"model":"m","state":{},"questions":{"q":{"type":"chat"}}}),
        json!({"model":"m","state":"text","questions":{"q":{"type":"choice"}}}),
        json!({"model":"m","state":{},"questions":{"q":{"type":"noul"}},"bodyFile":{"path":"/private"}}),
    ] {
        assert!(validate_body(&request(value)).is_err());
    }
    let mut oversized = request(question());
    oversized.body = Some("a".repeat(MAX_REQUEST.div_ceil(3) * 4 + 1));
    assert!(validate_body(&oversized)
        .unwrap_err()
        .message
        .contains("16 MiB"));
}

#[tokio::test]
async fn host_owned_body_files_are_verified_and_obey_the_decision_limit() {
    let directory = Directory::new();
    let path = directory.0.join("request.json");
    std::fs::write(&path, serde_json::to_vec(&question()).unwrap()).unwrap();
    let descriptor = nora_provider_plugin_sdk::RequestBodyFile::from_path(&path).unwrap();
    let mut call = request(question());
    call.body = None;
    call.body_file = Some(descriptor.clone());
    let client = Client::new(vec![answer()]);
    fetch(&definition(), call, &client).await.unwrap();
    let mut call = request(question());
    call.body_file = Some(descriptor.clone());
    assert!(validate_body(&call).is_err());
    let mut call = request(question());
    call.body = None;
    call.body_file = Some(nora_provider_plugin_sdk::RequestBodyFile {
        length: MAX_REQUEST as u64 + 1,
        ..descriptor.clone()
    });
    assert!(validate_body(&call).unwrap_err().message.contains("16 MiB"));
    std::fs::write(&path, b"changed").unwrap();
    let mut call = request(question());
    call.body = None;
    call.body_file = Some(descriptor);
    assert!(validate_body(&call).is_err());
}

#[tokio::test]
async fn initialization_authorization_and_unavailable_quota_use_the_sdk_contract() {
    let directory = Directory::new();
    let provider = Typesafe::default();
    let manifest: Value = serde_json::from_str(include_str!("../nora-provider.json")).unwrap();
    provider
        .initialize(InitializeParams {
            api_version: API_VERSION,
            plugin_id: "typesafe".into(),
            plugin_directory: directory.0.clone(),
            data_directory: directory.0.clone(),
            manifest,
        })
        .await
        .unwrap();
    let auth = provider
        .authorize(json!({"method":"apiKey","inputs":{"apiKey":" private-typesafe-key "}}))
        .await
        .unwrap();
    assert_eq!(auth["credentials"]["access"], "private-typesafe-key");
    assert!(provider.authorize(json!({"method":"oauth"})).await.is_err());
    let usage = provider.usage(context()).await.unwrap();
    assert_eq!(usage["available"], false);
    assert!(usage["items"].as_array().unwrap().is_empty());
    assert!(usage["message"]
        .as_str()
        .unwrap()
        .contains("does not expose"));
}

#[tokio::test]
async fn oauth_credentials_never_authorize_a_systemone_call() {
    let client = Client::new(vec![]);
    let mut call = request(question());
    call.credentials = Some(json!({"type":"oauth","access":"private-oauth-token"}));
    assert!(fetch(&definition(), call, &client).await.is_err());
    assert!(client.requests.lock().unwrap().is_empty());
}
