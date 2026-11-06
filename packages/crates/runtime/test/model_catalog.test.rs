use super::*;
use async_trait::async_trait;
use futures::stream;
use nora_ai::{HttpClientError, HttpStreamingResponse};
use serde_json::json;
use std::collections::VecDeque;

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "nora-model-catalog-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn catalog_path(&self) -> PathBuf {
        self.0.join("models.json")
    }

    fn save(&self, payload: &Value) -> Vec<u8> {
        let bytes = serde_json::to_vec(payload).unwrap();
        std::fs::write(self.catalog_path(), &bytes).unwrap();
        bytes
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Reply {
    status: u16,
    body: Vec<u8>,
    interrupted: bool,
}

impl Reply {
    fn json(payload: Value) -> Self {
        Self {
            status: 200,
            body: serde_json::to_vec(&payload).unwrap(),
            interrupted: false,
        }
    }

    fn failure() -> Self {
        Self {
            status: 503,
            body: b"private upstream detail".to_vec(),
            interrupted: false,
        }
    }
}

struct MockClient {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<Reply>>,
}

impl MockClient {
    fn new(replies: Vec<Reply>) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            replies: Mutex::new(replies.into()),
        }
    }

    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

#[async_trait]
impl HttpClient for MockClient {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected model catalog request");
        let mut chunks = vec![Ok(reply.body.into())];
        if reply.interrupted {
            chunks.push(Err(HttpClientError::InvalidResponse));
        }
        Ok(HttpStreamingResponse {
            status: reply.status,
            headers: BTreeMap::new(),
            body: Box::pin(stream::iter(chunks)),
        })
    }
}

fn payload(id: &str) -> Value {
    json!({
        "vendor": {
            "id": "vendor",
            "name": "Vendor",
            "api": "https://vendor.example/v1",
            "npm": "@ai-sdk/openai-compatible",
            "models": {
                id: {
                    "id": id,
                    "name": "Fresh Model",
                    "reasoning": true,
                    "release_date": "2026-10-21",
                    "tool_call": true,
                    "temperature": false,
                    "limit": {"context": 200000,"input": 150000,"output": 32000},
                    "modalities": {"input": ["text","image"],"output": ["text"]},
                    "cost": {"input": 2.0,"output": 8.0,"cache_read": 0.2,"cache_write": 2.5}
                }
            }
        }
    })
}

#[tokio::test]
async fn a_cold_catalog_is_downloaded_without_credentials_and_saved_whole() {
    let directory = TempDirectory::new();
    let value = payload("new-model");
    let client = MockClient::new(vec![Reply::json(value.clone())]);
    let catalog = load(&client, &directory.0).await.unwrap();
    let model = catalog.model("vendor", "new-model").unwrap();
    assert_eq!(model.name, "Fresh Model");
    assert_eq!(model.base_url, "https://vendor.example/v1");
    assert_eq!(model.api, "openai-completions");
    assert_eq!(model.context_window, 150000);
    assert_eq!(model.max_tokens, 32000);
    assert_eq!(model.input, vec![InputModality::Text, InputModality::Image]);
    assert!(model.reasoning);
    assert_eq!(model.info.release_date.as_deref(), Some("2026-10-21"));
    assert_eq!(model.info.tool_calling, Some(true));
    assert_eq!(model.info.temperature, Some(false));
    assert_eq!(model.cost.cache_write, 2.5);
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(directory.catalog_path()).unwrap()).unwrap(),
        value
    );
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, HttpMethod::Get);
    assert_eq!(requests[0].url, CATALOG_URL);
    assert_eq!(requests[0].headers.len(), 2);
    assert!(requests[0].body.is_none());
    assert!(!requests[0].headers.contains_key("authorization"));
}

#[tokio::test]
async fn a_fresh_saved_catalog_is_used_without_a_network_request() {
    let directory = TempDirectory::new();
    directory.save(&payload("saved-model"));
    let client = MockClient::new(Vec::new());
    let now = SystemTime::now();
    for offset in [Duration::ZERO, CHECK_EVERY, CHECK_EVERY * 2] {
        let catalog = load_at(&client, &directory.0, now + offset).await.unwrap();
        assert!(catalog.model("vendor", "saved-model").is_some());
    }
    assert_eq!(client.request_count(), 0);
}

#[test]
fn cached_only_reads_the_saved_catalog_and_never_needs_a_download() {
    let directory = TempDirectory::new();
    assert!(cached(&directory.0).is_err());
    directory.save(&payload("saved-model"));
    assert!(cached(&directory.0)
        .unwrap()
        .model("vendor", "saved-model")
        .is_some());
}

#[tokio::test]
async fn cached_is_available_while_a_refresh_is_in_progress() {
    let directory = TempDirectory::new();
    directory.save(&payload("saved-model"));
    let shared = cache_for(&directory.0).unwrap();
    let _refresh = shared.lock().await;
    assert!(cached(&directory.0)
        .unwrap()
        .model("vendor", "saved-model")
        .is_some());
}

#[tokio::test]
async fn a_stale_catalog_survives_failure_and_retries_at_the_next_hour() {
    let directory = TempDirectory::new();
    let original = directory.save(&payload("old-model"));
    let client = MockClient::new(vec![Reply::failure(), Reply::json(payload("new-model"))]);
    let now = SystemTime::now() + STALE_AFTER + Duration::from_secs(1);
    let catalog = load_at(&client, &directory.0, now).await.unwrap();
    assert!(catalog.model("vendor", "old-model").is_some());
    assert_eq!(std::fs::read(directory.catalog_path()).unwrap(), original);
    let catalog = load_at(&client, &directory.0, now + Duration::from_secs(300))
        .await
        .unwrap();
    assert!(catalog.model("vendor", "old-model").is_some());
    assert_eq!(client.request_count(), 1);
    let catalog = load_at(&client, &directory.0, now + CHECK_EVERY)
        .await
        .unwrap();
    assert!(catalog.model("vendor", "new-model").is_some());
    assert!(catalog.model("vendor", "old-model").is_none());
    assert_eq!(client.request_count(), 2);
}

#[tokio::test]
async fn an_unreadable_or_empty_download_never_replaces_the_saved_catalog() {
    for reply in [
        Reply::json(json!({})),
        Reply::json(json!([])),
        Reply::json(json!({"vendor":{"models":{}}})),
        Reply::json(json!({"error":"private upstream detail"})),
        Reply {
            status: 200,
            body: b"{not-json".to_vec(),
            interrupted: false,
        },
        Reply {
            status: 200,
            body: serde_json::to_vec(&payload("half-download")).unwrap(),
            interrupted: true,
        },
    ] {
        let directory = TempDirectory::new();
        let original = directory.save(&payload("old-model"));
        let client = MockClient::new(vec![reply]);
        let catalog = load_at(
            &client,
            &directory.0,
            SystemTime::now() + STALE_AFTER + Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert!(catalog.model("vendor", "old-model").is_some());
        assert_eq!(std::fs::read(directory.catalog_path()).unwrap(), original);
    }
}

#[tokio::test]
async fn a_failed_first_download_has_no_compiled_fallback_and_is_bounded() {
    let directory = TempDirectory::new();
    let client = MockClient::new(vec![Reply::failure(), Reply::json(payload("recovered"))]);
    let now = SystemTime::now();
    let error = load_at(&client, &directory.0, now).await.unwrap_err();
    assert_eq!(error.code, "upstream_transport_error");
    assert!(!error.message.contains("private"));
    assert!(!directory.catalog_path().exists());
    assert!(
        load_at(&client, &directory.0, now + Duration::from_secs(300))
            .await
            .is_err()
    );
    assert_eq!(client.request_count(), 1);
    let catalog = load_at(&client, &directory.0, now + CHECK_EVERY)
        .await
        .unwrap();
    assert!(catalog.model("vendor", "recovered").is_some());
}

#[tokio::test]
async fn concurrent_reads_of_one_directory_share_the_download() {
    let directory = TempDirectory::new();
    let client = MockClient::new(vec![Reply::json(payload("new-model"))]);
    let (left, right) = tokio::join!(load(&client, &directory.0), load(&client, &directory.0));
    assert_eq!(left.unwrap(), right.unwrap());
    assert_eq!(client.request_count(), 1);
}

#[tokio::test]
async fn cancelling_a_download_does_not_delay_the_next_attempt_by_an_hour() {
    struct PendingClient;
    #[async_trait]
    impl HttpClient for PendingClient {
        async fn stream(&self, _: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
            std::future::pending().await
        }
    }

    let directory = TempDirectory::new();
    assert!(
        tokio::time::timeout(Duration::from_millis(1), load(&PendingClient, &directory.0))
            .await
            .is_err()
    );
    let client = MockClient::new(vec![Reply::json(payload("retried-model"))]);
    assert!(load(&client, &directory.0)
        .await
        .unwrap()
        .model("vendor", "retried-model")
        .is_some());
}

#[tokio::test]
async fn provider_directories_never_share_their_catalog_state() {
    let first = TempDirectory::new();
    let second = TempDirectory::new();
    let client = MockClient::new(vec![
        Reply::json(payload("first-only")),
        Reply::json(payload("second-only")),
    ]);
    let left = load(&client, &first.0).await.unwrap();
    let right = load(&client, &second.0).await.unwrap();
    assert!(left.model("vendor", "first-only").is_some());
    assert!(left.model("vendor", "second-only").is_none());
    assert!(right.model("vendor", "second-only").is_some());
    assert!(right.model("vendor", "first-only").is_none());
}

#[tokio::test]
async fn an_invalid_local_catalog_is_replaced_only_by_a_valid_download() {
    let directory = TempDirectory::new();
    std::fs::write(directory.catalog_path(), b"invalid").unwrap();
    let client = MockClient::new(vec![Reply::json(payload("valid-model"))]);
    assert!(load(&client, &directory.0)
        .await
        .unwrap()
        .model("vendor", "valid-model")
        .is_some());
    assert!(decode(&std::fs::read(directory.catalog_path()).unwrap()).is_ok());
}

#[test]
fn models_dev_rows_preserve_each_models_protocol_and_keep_non_chat_models_separate() {
    let value = json!({
        "relay": {
            "npm":"@ai-sdk/openai-compatible",
            "api":"https://relay.example/v1",
            "models": {
                "claude-new": {
                    "id":"claude-new","name":"Claude New",
                    "provider":{"npm":"@ai-sdk/anthropic","api":"https://relay.example/messages"},
                    "reasoning_options":[{"type":"effort","values":["low","high"]}],
                    "limit":{"context":100000,"input":200000}
                },
                "gpt-new": {"id":"gpt-new","provider":{"npm":"@ai-sdk/openai"}},
                "tiny-limit": {"id":"tiny-limit","limit":{"context":512,"input":100000}},
                "no-limits": {"id":"no-limits"},
                "text-embed": {"id":"text-embed","modalities":{"output":["text"]}},
                "voice": {"id":"voice","modalities":{"output":["audio"]}},
                "gpt-image-new": {"id":"gpt-image-new"}
            }
        }
    });
    let catalog = decode(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(catalog.model_count(), 7);
    let embedding = catalog.model("relay", "text-embed").unwrap();
    assert_eq!(embedding.info.chat, Some(false));
    assert_eq!(embedding.info.embedding, Some(true));
    assert_eq!(
        catalog.model("relay", "voice").unwrap().info.chat,
        Some(false)
    );
    let image = catalog.model("relay", "gpt-image-new").unwrap();
    assert_eq!(image.info.chat, Some(false));
    assert_eq!(image.info.image_output, Some(true));
    let claude = catalog.model("relay", "claude-new").unwrap();
    assert_eq!(claude.api, "anthropic-messages");
    assert_eq!(claude.base_url, "https://relay.example/messages");
    assert_eq!(claude.context_window, 100000);
    assert!(claude.reasoning);
    assert_eq!(claude.info.reasoning_efforts, vec!["low", "high"]);
    assert_eq!(
        catalog.model("relay", "gpt-new").unwrap().api,
        "openai-responses"
    );
    assert_eq!(
        catalog.model("relay", "tiny-limit").unwrap().context_window,
        100000
    );
    assert_eq!(
        catalog.model("relay", "no-limits").unwrap().context_window,
        0
    );
}

#[test]
fn actual_models_dev_rows_keep_pdf_and_video_inputs() {
    // Exact rows from Magpie's models.dev 2026-10-09 fixture, without adding
    // capabilities or inferred limits to its catalog entries.
    let catalog = decode(include_bytes!(
        "fixtures/models-dev-multimodal-2026-10-09.json"
    ))
    .unwrap();
    assert_eq!(
        catalog.model("anthropic", "claude-sonnet-5").unwrap().input,
        vec![
            InputModality::Text,
            InputModality::Image,
            InputModality::Pdf
        ]
    );
    assert_eq!(
        catalog.model("kimi-code-plan-global", "k3").unwrap().input,
        vec![
            InputModality::Text,
            InputModality::Image,
            InputModality::Video
        ]
    );
}

#[test]
fn all_supported_input_modalities_survive_in_the_sources_order_without_adding_text() {
    let value = json!({"vendor":{"models":{
        "omni":{"id":"omni","modalities":{"input":["pdf","audio","video","image","text","audio"],"output":["text","audio"]}},
        "audio-chat":{"id":"audio-chat","modalities":{"input":["audio"],"output":["text","audio"]}},
        "video-understanding":{"id":"video-understanding","modalities":{"input":["video"],"output":["text"]}},
        "pdf-only":{"id":"pdf-only","modalities":{"input":["pdf"],"output":["text"]}}
    }}});
    let catalog = decode(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(
        catalog.model("vendor", "omni").unwrap().input,
        vec![
            InputModality::Pdf,
            InputModality::Audio,
            InputModality::Video,
            InputModality::Image,
            InputModality::Text
        ]
    );
    for (id, input) in [
        ("audio-chat", InputModality::Audio),
        ("video-understanding", InputModality::Video),
        ("pdf-only", InputModality::Pdf),
    ] {
        let model = catalog.model("vendor", id).unwrap();
        assert_eq!(model.input, vec![input]);
        assert_eq!(model.info.chat, Some(true));
    }
}

#[test]
fn native_kinds_and_explicit_non_chat_flags_override_text_output_guesses() {
    let value = json!({"vendor":{"models":{
        "vectors":{"kind":"embedding","modalities":{"output":["text"]}},
        "draw":{"kind":"image_generation","modalities":{"output":["text","image"]}},
        "film":{"kind":"video","modalities":{"output":["video"]}},
        "listen":{"kind":"transcription","modalities":{"input":["audio"],"output":["text"]}},
        "speak":{"kind":"speech","modalities":{"input":["text"],"output":["audio"]}},
        "sort":{"capabilities":{"type":"rerank"},"modalities":{"output":["text"]}},
        "decide":{"kind":"decision","modalities":{"output":["text"]}},
        "explicit":{"capabilities":{"completion_chat":false},"modalities":{"output":["text"]}},
        "unknown":{"kind":"future-inference-kind","modalities":{"output":["text"]}},
        "chat-with-audio-name":{"kind":"chat","modalities":{"output":["text"]}}
    }}});
    let catalog = decode(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(catalog.model_count(), 10);
    for (id, kind) in [
        ("vectors", "embedding"),
        ("draw", "image_generation"),
        ("film", "video"),
        ("listen", "transcription"),
        ("speak", "speech"),
        ("sort", "rerank"),
        ("decide", "decision"),
    ] {
        let model = catalog.model("vendor", id).unwrap();
        assert_eq!(model.info.native_kind.as_deref(), Some(kind));
        assert_eq!(model.info.chat, Some(false), "{id}");
        assert_eq!(model.info.tool_calling, Some(false));
        assert!(
            model.provider_options.is_empty(),
            "Metadata must not enter requests"
        );
    }
    assert_eq!(
        catalog.model("vendor", "vectors").unwrap().info.embedding,
        Some(true)
    );
    assert_eq!(
        catalog.model("vendor", "draw").unwrap().info.image_output,
        Some(true)
    );
    assert_eq!(
        catalog.model("vendor", "film").unwrap().info.video_output,
        Some(true)
    );
    assert_eq!(
        catalog.model("vendor", "decide").unwrap().info.decision,
        Some(true)
    );
    assert_eq!(
        catalog.model("vendor", "listen").unwrap().input,
        vec![InputModality::Audio]
    );
    assert_eq!(
        catalog.model("vendor", "explicit").unwrap().info.chat,
        Some(false)
    );
    assert_eq!(
        catalog
            .model("vendor", "unknown")
            .unwrap()
            .info
            .native_kind
            .as_deref(),
        Some("future-inference-kind")
    );
    assert_eq!(
        catalog
            .model("vendor", "chat-with-audio-name")
            .unwrap()
            .info
            .chat,
        Some(true)
    );
}

#[test]
fn a_catalog_containing_only_non_chat_services_is_valid() {
    let value = json!({"vendor":{"models":{"speech-service":{"kind":"speech","modalities":{"input":["text"],"output":["audio"]}}}}});
    let catalog = decode(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(catalog.model_count(), 1);
    assert_eq!(
        catalog.model("vendor", "speech-service").unwrap().info.chat,
        Some(false)
    );
}

#[test]
fn reasoning_levels_follow_the_latest_older_family_member_and_preserve_budget_models() {
    let mut value = payload("base");
    value["vendor"]["models"] = json!({
        "old": {"id":"old","reasoning":true,"family":"line","release_date":"2026-01-01","reasoning_options":[{"type":"effort","values":["low"]}]},
        "new": {"id":"new","reasoning":true,"family":"line","release_date":"2026-02-01","reasoning_options":[{"type":"effort","values":["low","high"]}]},
        "next": {"id":"next","reasoning":true,"family":"line","release_date":"2026-03-01"},
        "future": {"id":"future","reasoning":true,"family":"line","release_date":"2026-04-01","reasoning_options":[{"type":"effort","values":["max"]}]},
        "budget": {"id":"budget","reasoning":true,"family":"line","release_date":"2026-03-01","reasoning_options":[{"type":"budget_tokens"}]}
    });
    let catalog = decode(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(
        catalog
            .model("vendor", "next")
            .unwrap()
            .info
            .reasoning_efforts,
        vec!["low", "high"]
    );
    assert!(catalog
        .model("vendor", "budget")
        .unwrap()
        .info
        .reasoning_efforts
        .is_empty());
}

#[test]
fn reasoning_off_is_only_added_for_the_providers_magpie_has_verified() {
    let row =
        json!({"reasoning_options":[{"type":"toggle"},{"type":"effort","values":["low","high"]}]});
    assert_eq!(
        explicit_reasoning_levels("deepseek", &row),
        vec!["none", "low", "high"]
    );
    assert_eq!(
        explicit_reasoning_levels("kimi-code-plan-cn", &row),
        vec!["none", "low", "high"]
    );
    assert_eq!(
        explicit_reasoning_levels("other", &row),
        vec!["low", "high"]
    );
}

#[test]
fn atomic_persistence_failure_leaves_no_partial_catalog_or_temporary_file() {
    let directory = TempDirectory::new();
    std::fs::create_dir(directory.catalog_path()).unwrap();
    assert!(persist(&directory.catalog_path(), b"new catalog").is_err());
    assert!(directory.catalog_path().is_dir());
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
}
