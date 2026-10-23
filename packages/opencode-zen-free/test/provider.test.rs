use super::*;
use base64::Engine;
use nora_ai::{HttpClientError, HttpStreamingResponse};
use std::{collections::VecDeque, path::PathBuf, sync::Mutex as StdMutex};
struct Reply {
    path: &'static str,
    status: u16,
    kind: &'static str,
    bytes: Vec<u8>,
}
fn json_reply(path: &'static str, status: u16, value: Value) -> Reply {
    Reply {
        path,
        status,
        kind: "application/json",
        bytes: serde_json::to_vec(&value).unwrap(),
    }
}
fn sse_reply() -> Reply {
    let value = json!({"id":"reply","choices":[{"index":0,"delta":{"content":"actual"},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":2}});
    Reply {
        path: "/chat/completions",
        status: 200,
        kind: "text/event-stream",
        bytes: format!("data: {value}\n\ndata: [DONE]\n\n").into_bytes(),
    }
}
struct Client {
    requests: StdMutex<Vec<HttpRequest>>,
    replies: StdMutex<VecDeque<Reply>>,
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        let mut replies = self.replies.lock().unwrap();
        let index = replies
            .iter()
            .position(|reply| request.url.ends_with(reply.path))
            .unwrap_or_else(|| panic!("Unexpected Zen request: {}", request.url));
        let reply = replies.remove(index).unwrap();
        drop(replies);
        self.requests.lock().unwrap().push(request);
        Ok(HttpStreamingResponse {
            status: reply.status,
            headers: BTreeMap::from([("content-type".into(), reply.kind.into())]),
            body: Box::pin(futures::stream::once(async move { Ok(reply.bytes.into()) })),
        })
    }
}
struct Store;
#[async_trait]
impl core::CredentialStore for Store {
    async fn save(&self, _: Value) -> PluginResult<()> {
        panic!("Public access must not rotate a private credential")
    }
}
fn setup(replies: Vec<Reply>) -> (ZenFree, Arc<Client>) {
    let client = Arc::new(Client {
        requests: StdMutex::new(Vec::new()),
        replies: StdMutex::new(replies.into()),
    });
    let mut plugin = ZenFree::default();
    plugin.core = Core::with_dependencies("opencode-zen-free", client.clone(), Arc::new(Store));
    plugin
        .core
        .initialize(InitializeParams {
            api_version: 1,
            plugin_id: "opencode-zen-free".into(),
            plugin_directory: PathBuf::new(),
            data_directory: PathBuf::new(),
            manifest: serde_json::from_str(include_str!("../nora-provider.json")).unwrap(),
        })
        .unwrap();
    (plugin, client)
}
fn credentials() -> Value {
    json!({"type":"api","key":"public","provider":"opencode-zen-free","authMethod":"public"})
}
fn request(streaming: bool) -> FetchRequest {
    FetchRequest{account:"public".into(),credentials:Some(credentials()),network_proxy:None,url:format!("{}/chat/completions",catalog::BASE),method:"POST".into(),headers:BTreeMap::from([("user-agent".into(),"untrusted-client".into()),("authorization".into(),"Bearer private-key".into())]),metadata:BTreeMap::from([("sessionID".into(),"private-conversation-id".into())]),body:Some(base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&json!({"model":"cat-jam-free","messages":[{"role":"user","content":"hello"}],"stream":streaming,"top_p":0.4,"stop":["END"],"tools":[{"type":"function","function":{"name":"my_tool","parameters":{"type":"object"}}}]})).unwrap())),body_file:None}
}
async fn response_json(response: FetchResponse) -> Value {
    let bytes = response
        .body
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<PluginResult<Vec<_>>>()
        .unwrap()
        .concat();
    serde_json::from_slice(&bytes).unwrap()
}
fn discovery(ids: Vec<&str>) -> Vec<Reply> {
    vec![
        json_reply(
            "/models",
            200,
            json!({"data":ids.into_iter().map(|id|json!({"id":id})).collect::<Vec<_>>()}),
        ),
        json_reply("/api.json", 503, json!({})),
        Reply {
            path: "/models.mdx",
            status: 503,
            kind: "text/plain",
            bytes: vec![],
        },
    ]
}
#[tokio::test]
async fn public_access_is_verified_and_private_keys_are_not_accepted() {
    let (plugin, client) = setup(vec![json_reply("/models", 200, json!({"data":[]}))]);
    assert!(plugin
        .authorize(json!({"authMethodID":"public","inputs":{"key":"private-key"}}))
        .await
        .is_err());
    let value = plugin
        .authorize(json!({"authMethodID":"public"}))
        .await
        .unwrap();
    assert_eq!(value["credentials"]["key"], "public");
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    assert_eq!(
        client.requests.lock().unwrap()[0].headers["authorization"],
        "Bearer public"
    );
}
#[tokio::test]
async fn live_listing_removes_retired_bootstrap_ids_and_includes_new_free_models_when_metadata_is_down(
) {
    let (plugin, _) = setup(discovery(vec!["brand-new-free"]));
    let result = plugin
        .models(CallContext {
            account: "public".into(),
            credentials: Some(credentials()),
            network_proxy: None,
        })
        .await
        .unwrap();
    assert_eq!(result["source"], "remote");
    assert_eq!(result["models"].as_array().unwrap().len(), 1);
    assert_eq!(result["models"][0]["id"], "brand-new-free");
    assert_eq!(result["models"][0]["api"], "openai-completions");
}
#[tokio::test]
async fn native_chat_uses_official_headers_and_standins_but_preserves_sampling_and_user_tools() {
    let (plugin, client) = setup(vec![sse_reply()]);
    let result = response_json(plugin.fetch(request(false)).await.unwrap()).await;
    assert_eq!(result["choices"][0]["message"]["content"], "actual");
    assert_eq!(result["usage"]["prompt_tokens"], 5);
    let requests = client.requests.lock().unwrap();
    let outgoing = &requests[0];
    let body: Value = serde_json::from_slice(outgoing.body.as_ref().unwrap()).unwrap();
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(body["top_p"], 0.4);
    assert_eq!(body["stop"], json!(["END"]));
    assert_eq!(body["tools"].as_array().unwrap().len(), 6);
    assert_eq!(body["tools"][0]["function"]["name"], "my_tool");
    assert_eq!(outgoing.headers["authorization"], "Bearer public");
    assert!(outgoing.headers["user-agent"].contains("provider-utils/4.0.23"));
    assert!(crate::session::real(
        &outgoing.headers["x-opencode-session"]
    ));
    assert!(!outgoing
        .headers
        .values()
        .any(|value| value.contains("private-conversation")));
}
#[tokio::test]
async fn inference_rewrites_only_the_official_legacy_endpoint() {
    let (plugin, client) = setup(vec![sse_reply()]);
    let mut legacy = request(false);
    legacy.url = format!("{}/chat/completions", catalog::LEGACY);
    let _ = response_json(plugin.fetch(legacy).await.unwrap()).await;
    assert_eq!(
        client.requests.lock().unwrap()[0].url,
        format!("{}/chat/completions", catalog::BASE)
    );
    let mut foreign = request(false);
    foreign.url = "https://evil.example/inference/v1/chat/completions".into();
    assert!(plugin.fetch(foreign).await.is_err());
}
#[tokio::test]
async fn protocol_mismatches_are_rejected_before_a_request_and_public_quota_is_not_invented() {
    let (plugin, client) = setup(vec![]);
    let mut wrong = request(true);
    let body = json!({"model":"muse-spark-1.3-contributor-free","messages":[]});
    wrong.body =
        Some(base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&body).unwrap()));
    assert!(plugin.fetch(wrong).await.is_err());
    assert!(client.requests.lock().unwrap().is_empty());
    let usage = plugin
        .usage(CallContext {
            account: "public".into(),
            credentials: Some(credentials()),
            network_proxy: None,
        })
        .await
        .unwrap();
    assert_eq!(usage["available"], false);
    assert!(usage["items"].as_array().unwrap().is_empty());
}
#[tokio::test]
async fn model_misses_relist_at_most_once_per_minute_and_return_404_without_expiring_public_access()
{
    let (plugin, client) = setup(discovery(vec!["cat-jam-free"]));
    for _ in 0..2 {
        let mut missing = request(false);
        missing.body = Some(
            base64::engine::general_purpose::STANDARD
                .encode(br#"{"model":"nonexistent-free","messages":[]}"#),
        );
        assert_eq!(plugin.fetch(missing).await.unwrap().status, 404);
    }
    assert_eq!(client.requests.lock().unwrap().len(), 3);
}
#[tokio::test]
async fn concurrency_permits_live_until_stream_completion_or_cancellation() {
    let (plugin, client) = setup(vec![sse_reply(), sse_reply(), sse_reply()]);
    let first = plugin.fetch(request(true)).await.unwrap();
    let second = plugin.fetch(request(true)).await.unwrap();
    let third = plugin.fetch(request(true));
    tokio::pin!(third);
    assert!(tokio::time::timeout(Duration::from_millis(20), &mut third)
        .await
        .is_err());
    assert_eq!(client.requests.lock().unwrap().len(), 2);
    drop(first);
    let third = tokio::time::timeout(Duration::from_secs(1), &mut third)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(client.requests.lock().unwrap().len(), 3);
    drop(second);
    drop(third);
}
#[tokio::test]
async fn nonstream_truncation_is_a_failure_instead_of_a_success_with_an_empty_answer() {
    let (plugin, _) = setup(vec![Reply {
        path: "/chat/completions",
        status: 200,
        kind: "text/event-stream",
        bytes: b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n\n"
            .to_vec(),
    }]);
    let result = plugin.fetch(request(false)).await.unwrap();
    assert_eq!(result.status, 502);
    let body = response_json(result).await;
    assert!(body.get("error").is_some());
}
#[test]
fn anthropic_headers_use_only_public_auth_and_preserve_explicit_beta() {
    let (plugin, _) = setup(vec![]);
    let mut request = request(true);
    request.headers.insert(
        "anthropic-beta".into(),
        "interleaved-thinking-2025-05-14".into(),
    );
    let headers = plugin.headers(&request, &json!({}), Protocol::Anthropic);
    assert_eq!(headers["x-api-key"], "public");
    assert!(!headers.contains_key("authorization"));
    assert_eq!(headers["anthropic-beta"], "interleaved-thinking-2025-05-14");
    assert!(headers["user-agent"].contains("provider-utils/4.0.46"));
}
