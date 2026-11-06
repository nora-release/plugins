use super::*;
use async_trait::async_trait;
use futures::stream;
use nora_ai::{
    HttpClientError, HttpStreamingResponse, JsonValue, OAuthCredentials, OAuthProviderId,
};
use std::collections::VecDeque;
use std::sync::Mutex;

struct Reply {
    status: u16,
    body: Vec<u8>,
}

#[tokio::test]
async fn declared_model_protocols_survive_discovery_without_becoming_request_options() {
    let definition = definition("openai-completions", "https://relay.invalid/v1");
    let client = client(vec![Reply::json(json!({"data":[
        {"id":"claude","supported_endpoints":["/v1/messages/"]},
        {"id":"chat","supported_endpoints":["/chat/completions","/responses","/responses/"]},
        {"id":"native","supported_endpoints":["/chat/completions"],"native_endpoints":["/messages"]},
        {"id":"family","type_target":"anthropic"},
        {"id":"unknown"}, {"id":"empty","supported_endpoints":[]},
        {"id":"websocket","supported_endpoints":["ws:/responses"]}
    ]}))]);
    let directory =
        std::env::temp_dir().join(format!("nora-protocol-discovery-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let result = discover(&definition, Some(&api_credentials()), &directory, &client)
        .await
        .unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    assert_eq!(
        result["supportedAPIs"]["claude"],
        json!(["anthropic-messages"])
    );
    assert_eq!(
        result["supportedAPIs"]["chat"],
        json!(["openai-completions", "openai-responses"])
    );
    assert_eq!(
        result["supportedAPIs"]["native"],
        json!(["anthropic-messages"])
    );
    assert_eq!(
        result["supportedAPIs"]["family"],
        json!(["anthropic-messages"])
    );
    for id in ["unknown", "empty", "websocket"] {
        assert!(result["supportedAPIs"].get(id).is_none());
    }
    assert_eq!(
        result["apiEndpoints"],
        json!({"openai-completions":"https://relay.invalid/v1"})
    );
    for model in result["models"].as_array().unwrap() {
        assert!(model["providerOptions"].get("supportedAPIs").is_none());
    }
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn remote_free_and_price_fields_are_model_information_and_never_request_parameters() {
    let definition = definition("openai-completions", "https://example.test/v1");
    let client = client(vec![Reply::json(json!({"data":[
        {"id":"marked-free","free":true},
        {"id":"zero-priced","pricing":{"prompt":"0","completion":"0"}},
        {"id":"paid","pricing":{"prompt":"0.000003","completion":"0.000015"}},
        {"id":"unknown"}
    ]}))]);
    let result = fetch_routes(
        &definition,
        &api_credentials(),
        &resolved(),
        &catalog(),
        &client,
    )
    .await
    .unwrap();
    let find = |id: &str| result.models.iter().find(|model| model.id == id).unwrap();
    assert_eq!(find("marked-free").info.free, Some(true));
    assert_eq!(find("zero-priced").info.pricing_known, Some(true));
    assert_eq!(find("paid").cost.input, 3.0);
    assert_eq!(find("paid").cost.output, 15.0);
    assert_eq!(find("unknown").info.pricing_known, None);
    for model in &result.models {
        assert!(!model.provider_options.contains_key("free"));
        assert!(!model.provider_options.contains_key("pricingKnown"));
    }
}

#[tokio::test]
async fn real_model_discovery_keeps_context_prices_and_claude_hour_write_rules() {
    let definition = definition("anthropic-messages", "https://price-relay.invalid");
    let client = client(vec![Reply::json(
        json!({"data":[{"id":"claude-current","cost":{"input":5,"output":25,"cache_write":6.25,"tiers":[{"tier":{"type":"context","size":200000},"input":10,"output":37.5}]}}]}),
    )]);
    let result = fetch_routes(
        &definition,
        &api_credentials(),
        &resolved(),
        &catalog(),
        &client,
    )
    .await
    .unwrap();
    let model = &result.models[0];
    assert_eq!(model.info.pricing_known, Some(true));
    assert_eq!(
        model
            .info
            .cache_write_1h
            .as_ref()
            .and_then(serde_json::Number::as_f64),
        Some(10.0)
    );
    assert_eq!(model.info.pricing_tiers[0].above, 200000);
    assert_eq!(
        model.info.pricing_tiers[0]
            .cache_write_1h
            .as_ref()
            .and_then(serde_json::Number::as_f64),
        Some(20.0)
    );
    assert!(model.provider_options.is_empty());
}

#[tokio::test]
async fn model_list_header_learning_is_shared_with_later_fetches_for_only_that_account() {
    let mut definition = definition("anthropic-messages", "https://learn-relay.invalid");
    definition.id = "list-header-learning".into();
    definition.request_account = Some("first".into());
    let client = client(vec![
        Reply {
            status: 401,
            body: br#"{"error":"x-api-key and Authorization cannot both be present"}"#.to_vec(),
        },
        Reply::json(json!({"data":[{"id":"one"}]})),
    ]);
    fetch_routes(
        &definition,
        &api_credentials(),
        &resolved(),
        &catalog(),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(
        crate::auth_headers::choice(
            &definition,
            &api_credentials(),
            "https://learn-relay.invalid/messages"
        ),
        Some("x-api-key")
    );
    definition.request_account = Some("second".into());
    assert_eq!(
        crate::auth_headers::choice(
            &definition,
            &api_credentials(),
            "https://learn-relay.invalid/messages"
        ),
        None
    );
}
impl Reply {
    fn json(value: Value) -> Self {
        Self {
            status: 200,
            body: serde_json::to_vec(&value).unwrap(),
        }
    }
    fn status(status: u16) -> Self {
        Self {
            status,
            body: br#"{"error":"private-token"}"#.to_vec(),
        }
    }
}
struct MockClient {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<Reply>>,
    stall_at: Option<usize>,
}
#[async_trait]
impl HttpClient for MockClient {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        let index = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request);
            requests.len()
        };
        if self.stall_at == Some(index) {
            futures::future::pending::<()>().await;
        }
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected model list request");
        Ok(HttpStreamingResponse {
            status: reply.status,
            headers: BTreeMap::new(),
            body: Box::pin(stream::iter(vec![Ok(reply.body.into())])),
        })
    }
}
fn client(replies: Vec<Reply>) -> MockClient {
    MockClient {
        requests: Mutex::new(Vec::new()),
        replies: Mutex::new(replies.into()),
        stall_at: None,
    }
}

#[tokio::test]
async fn a_total_deadline_keeps_successful_parts_and_marks_unread_protocols() {
    let mut definition = definition("openai-completions", "https://example.test/chat/v1");
    definition.config.model_discovery = Some(crate::definition::ModelDiscovery {
        default_base_url: definition.base_url.clone(),
        bases: vec![
            ModelBase {
                api: "openai-completions".into(),
                base_url: definition.base_url.clone(),
            },
            ModelBase {
                api: "anthropic-messages".into(),
                base_url: "https://example.test/anthropic".into(),
            },
            ModelBase {
                api: "openai-responses".into(),
                base_url: "https://example.test/responses/v1".into(),
            },
        ],
        no_list: false,
    });
    let client = MockClient {
        stall_at: Some(2),
        ..client(vec![Reply::json(json!({"data":[{"id":"good-model"}]}))])
    };
    let result = fetch_routes_with_budget(
        &definition,
        &api_credentials(),
        &resolved(),
        &catalog(),
        &client,
        Duration::from_millis(50),
    )
    .await
    .unwrap();
    assert_eq!(result.models.len(), 1);
    assert_eq!(result.models[0].id, "good-model");
    assert_eq!(
        result.failed_parts,
        [
            "anthropic-messages|https://example.test/anthropic",
            "openai-responses|https://example.test/responses/v1"
        ]
    );
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn an_expired_deadline_without_any_success_returns_an_error() {
    let definition = definition("openai-completions", "https://example.test/v1");
    let client = client(Vec::new());
    let result = fetch_routes_with_budget(
        &definition,
        &api_credentials(),
        &resolved(),
        &catalog(),
        &client,
        Duration::ZERO,
    )
    .await;
    assert!(result.is_err());
    assert!(client.requests.lock().unwrap().is_empty());
}
fn definition(api: &str, base: &str) -> Definition {
    Definition::from_fields(json!({"id":"fixture","name":"Fixture","api":api,"baseURL":base,"config":{"keyProvider":"fixture-key"}})).unwrap()
}
fn api_credentials() -> Value {
    json!({"type":"apiKey","access":"private-token"})
}
fn resolved() -> ResolvedProviderAuth {
    ResolvedProviderAuth {
        token: Some("private-token".into()),
        scheme: nora_ai::AuthScheme::Bearer,
        ..Default::default()
    }
}
fn catalog() -> ModelsCatalog {
    ModelsCatalog::default()
}
fn paths(client: &MockClient) -> Vec<String> {
    client
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| Url::parse(&request.url).unwrap().path().to_string())
        .collect()
}

#[test]
fn magpie_list_urls_and_versioned_endpoint_order_are_preserved() {
    let path_list = |base, kind| {
        list_urls(base, kind)
            .unwrap()
            .into_iter()
            .map(|url| url.path().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        path_list("https://example.test/anthropic", Kind::Anthropic),
        [
            "/anthropic/models",
            "/anthropic/v1/models",
            "/v1/models",
            "/models"
        ]
    );
    assert_eq!(
        path_list("https://example.test/api/coding/paas/v4", Kind::OpenAI),
        ["/api/coding/paas/v4/models"]
    );
    assert_eq!(
        path_list("https://example.test/v1", Kind::OpenAI),
        ["/v1/models"]
    );
    assert_eq!(
        path_list("https://example.test", Kind::OpenAI),
        ["/models", "/v1/models"]
    );
    assert_eq!(
        path_list("https://example.test/api", Kind::Anthropic),
        ["/api/models", "/api/v1/models", "/v1/models", "/models"]
    );
    for base in [
        "file:///tmp/catalog",
        "https://user:secret@example.test",
        "invalid",
    ] {
        assert!(list_urls(base, Kind::OpenAI).is_err());
    }
}

#[test]
fn codex_model_query_and_identity_headers_share_the_selected_version() {
    let mut definition = definition("openai-responses", "https://chatgpt.com");
    definition.adapter = crate::test_adapter("openai");
    let credentials: OAuthCredentials = serde_json::from_value(json!({
        "provider":"openAICodex", "access":"fixture-access", "refresh":"fixture-refresh",
        "expires":4102444800000i64, "extras":{"accountId":"fixture-account"}
    }))
    .unwrap();
    let mut resolved = crate::auth::base_auth("openai-codex", &credentials);
    definition.adapter.enrich_auth(&credentials, &mut resolved);
    let url = crate::openai_provider::model_urls_with_version("https://chatgpt.com", "0.170.2")
        .unwrap()
        .remove(0);
    let request = signed_request(url, &definition, &resolved, Kind::OpenAI, None, false);
    assert!(request.url.contains("client_version=0.170.2"));
    assert_eq!(request.headers["authorization"], "Bearer fixture-access");
    assert_eq!(request.headers["chatgpt-account-id"], "fixture-account");
    assert_eq!(request.headers["openai-beta"], "responses=experimental");
    for (name, value) in crate::openai_provider::codex_version::headers("0.170.2") {
        assert_eq!(request.headers[&name], value, "{name}");
    }
}

#[test]
fn openai_api_key_model_queries_do_not_acquire_codex_identity() {
    let mut definition = definition("openai-responses", "https://api.openai.com/v1");
    definition.adapter = crate::test_adapter("openai");
    let credentials: OAuthCredentials = serde_json::from_value(json!({
        "provider":"apiKey", "access":"fixture-key"
    }))
    .unwrap();
    let mut resolved = crate::auth::base_auth("openai", &credentials);
    definition.adapter.enrich_auth(&credentials, &mut resolved);
    let request = signed_request(
        Url::parse("https://api.openai.com/v1/models?client_version=0.170.2").unwrap(),
        &definition,
        &resolved,
        Kind::OpenAI,
        None,
        true,
    );
    assert_eq!(request.headers["authorization"], "Bearer fixture-key");
    assert_eq!(request.headers["user-agent"], "Nora");
    for name in ["originator", "version", "chatgpt-account-id", "openai-beta"] {
        assert!(!request.headers.contains_key(name), "{name}");
    }
}

#[tokio::test]
async fn generic_fetch_tries_magpie_candidates_and_only_returns_vendor_ids() {
    let definition = definition("openai-completions", "https://example.test");
    let client = client(vec![
        Reply::status(404),
        Reply::json(
            json!({"data":[{"id":"fresh-model","display_name":"Fresh Model","context_length":512000,"modalities":{"input":["text","image","audio","video","pdf"],"output":["text"]}},{"id":"text-embedding-new"}]}),
        ),
    ]);
    let mut catalog = catalog();
    catalog.insert(Model::new("old-model", "openai-completions", "openai"));
    let result = fetch_routes(
        &definition,
        &api_credentials(),
        &resolved(),
        &catalog,
        &client,
    )
    .await
    .unwrap();
    assert_eq!(paths(&client), ["/models", "/v1/models"]);
    assert_eq!(result.models.len(), 2);
    assert_eq!(result.models[1].id, "text-embedding-new");
    assert_eq!(result.models[1].info.embedding, Some(true));
    assert_eq!(result.models[1].info.chat, Some(false));
    assert_eq!(result.models[0].id, "fresh-model");
    assert_eq!(result.models[0].name, "Fresh Model");
    assert_eq!(result.models[0].context_window, 512000);
    assert!(result.models[0]
        .input
        .contains(&nora_ai::InputModality::Image));
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].headers["authorization"], "Bearer private-token");
    assert_eq!(requests[0].headers["x-api-key"], "private-token");
}

#[tokio::test]
async fn shared_list_urls_are_fetched_once_across_protocol_bases() {
    let mut definition = definition("anthropic-messages", "https://example.test");
    definition.config.model_discovery = Some(crate::definition::ModelDiscovery {
        default_base_url: definition.base_url.clone(),
        bases: vec![
            ModelBase {
                api: "openai-completions".into(),
                base_url: "https://example.test/v1".into(),
            },
            ModelBase {
                api: "anthropic-messages".into(),
                base_url: "https://example.test".into(),
            },
        ],
        no_list: false,
    });
    let client = client(vec![Reply::json(json!({"data":[{"id":"shared"}]}))]);
    let result = fetch_routes(
        &definition,
        &api_credentials(),
        &resolved(),
        &catalog(),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(paths(&client), ["/v1/models"]);
    assert_eq!(result.models.len(), 1);
    assert!(result.failed_parts.is_empty());
    assert_eq!(result.models[0].api, "openai-completions");
    assert_eq!(result.models[0].base_url, "https://example.test/v1");
    definition.base_url = "https://custom.test/private".into();
    let bases = configured_bases(&definition, Some(&api_credentials()));
    assert_eq!(bases.len(), 1);
    assert_eq!(bases[0].base_url, "https://custom.test/private");
}

#[tokio::test]
async fn a_failed_protocol_is_reported_so_the_host_keeps_that_previous_part() {
    let mut definition = definition("openai-completions", "https://example.test/chat/v1");
    definition.config.model_discovery = Some(crate::definition::ModelDiscovery {
        default_base_url: definition.base_url.clone(),
        bases: vec![
            ModelBase {
                api: "openai-completions".into(),
                base_url: definition.base_url.clone(),
            },
            ModelBase {
                api: "anthropic-messages".into(),
                base_url: "https://example.test/anthropic".into(),
            },
        ],
        no_list: false,
    });
    let client = client(vec![
        Reply::json(json!({"data":[{"id":"chat-new"}]})),
        Reply::status(503),
        Reply::status(503),
        Reply::status(503),
        Reply::status(503),
    ]);
    let result = fetch_routes(
        &definition,
        &api_credentials(),
        &resolved(),
        &catalog(),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(result.models[0].id, "chat-new");
    assert_eq!(
        result.failed_parts,
        ["anthropic-messages|https://example.test/anthropic"]
    );
}

#[tokio::test]
async fn anthropic_pages_are_complete_and_dual_auth_refusal_is_learned() {
    let definition = definition("anthropic-messages", "https://example.test");
    let client = client(vec![
        Reply {
            status: 401,
            body: br#"{"error":"x-api-key and Authorization cannot both be present"}"#.to_vec(),
        },
        Reply::json(json!({"data":[{"id":"first"}],"has_more":true,"last_id":"first"})),
        Reply::json(json!({"data":[{"id":"last"}],"has_more":false})),
    ]);
    let result = fetch_pages(
        Url::parse("https://example.test/v1/models").unwrap(),
        &definition,
        &api_credentials(),
        &resolved(),
        Kind::Anthropic,
        &BTreeMap::new(),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(
        result
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["first", "last"]
    );
    let requests = client.requests.lock().unwrap();
    assert!(requests[0].headers.contains_key("authorization"));
    for request in &requests[1..] {
        assert!(!request.headers.contains_key("authorization"));
        assert_eq!(request.headers["x-api-key"], "private-token");
        assert_eq!(request.headers["anthropic-version"], "2023-06-01");
    }
    let query = Url::parse(&requests[2].url)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect::<BTreeMap<_, _>>();
    assert_eq!(query["after_id"], "first");
    assert_eq!(query["limit"], "1000");
}

#[tokio::test]
async fn missing_repeated_or_failed_pages_never_publish_a_partial_list() {
    let definition = definition("openai-completions", "https://example.test/v1");
    for replies in [
        vec![Reply::json(
            json!({"data":[{"id":"first"}],"has_more":true}),
        )],
        vec![
            Reply::json(json!({"data":[{"id":"first"}],"has_more":true,"last_id":"same"})),
            Reply::json(json!({"data":[{"id":"first"}],"has_more":true,"last_id":"same"})),
        ],
        vec![
            Reply::json(json!({"data":[{"id":"first"}],"has_more":true,"last_id":"first"})),
            Reply::status(500),
        ],
    ] {
        let client = client(replies);
        let result = fetch_pages(
            Url::parse("https://example.test/v1/models").unwrap(),
            &definition,
            &api_credentials(),
            &resolved(),
            Kind::OpenAI,
            &BTreeMap::new(),
            &client,
        )
        .await;
        assert!(result.is_err());
        assert!(!result.unwrap_err().message.contains("private-token"));
    }
}

#[tokio::test]
async fn codex_uses_account_headers_priority_visibility_and_remote_metadata() {
    let mut definition = definition("openai-responses", "https://api.openai.com");
    definition.id = "openai".into();
    definition.adapter = crate::test_adapter("openai");
    definition.config.oauth_provider = Some("openai-codex".into());
    let value = json!({"type":"oauth","credentialProvider":"openAICodex","access":"private-token","extras":{"accountId":"account-123"}});
    let credentials = auth::decode(&definition, Some(&value)).unwrap();
    let resolved = auth::resolved_provider_auth_from_credentials("openai-codex", &credentials);
    let client = client(vec![Reply::json(
        json!({"models":[{"slug":"later","display_name":"Later","priority":9,"visibility":"list"},{"slug":"hidden","visibility":"hide"},{"slug":"fresh-codex","display_name":"Fresh Codex","priority":0,"visibility":"unlisted","context_window":200000,"max_context_window":1000000,"input_modalities":["text","image"],"supported_reasoning_levels":[{"effort":"low"},{"effort":"high"}],"service_tiers":[{"id":"priority"}]}]}),
    )]);
    let result = fetch_routes(&definition, &value, &resolved, &catalog(), &client)
        .await
        .unwrap();
    assert_eq!(
        result
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["fresh-codex", "later"]
    );
    let model = &result.models[0];
    assert_eq!(model.api, "chatgpt-codex");
    assert_eq!(model.context_window, 200000);
    assert!(model.reasoning);
    assert_eq!(model.info.max_context_window, Some(1000000u64));
    assert_eq!(model.info.service_tiers, vec!["priority"]);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].headers["chatgpt-account-id"], "account-123");
    assert!(requests[0]
        .url
        .contains("/backend-api/codex/models?client_version="));
    assert!(!serde_json::to_string(&result.models)
        .unwrap()
        .contains("private-token"));
}

#[tokio::test]
async fn grok_community_model_windows_and_efforts_match_the_plugin() {
    let mut definition = definition("openai-responses", "https://api.x.ai/v1");
    definition.id = "xai".into();
    definition.adapter = crate::test_adapter("xai");
    definition.config.oauth_provider = Some("xai".into());
    let value = json!({"type":"oauth","credentialProvider":"xAI","access":"private-token","extras":{"userId":"grok-user"}});
    let credentials = OAuthCredentials::new(
        OAuthProviderId::XAI,
        "private-token",
        None,
        None,
        BTreeMap::from([("userId".into(), JsonValue::string("grok-user"))]),
    );
    let resolved = auth::resolved_provider_auth_from_credentials("xai", &credentials);
    let client = client(vec![Reply::json(
        json!({"data":[{"id":"grok-new","name":"Grok New","api_backend":"responses","context_window":256000,"context_windows":[256000,500000,131072],"reasoning_efforts":[{"value":"high"},{"value":"low"}]},{"id":"wrong-api","api_backend":"chat"}]}),
    )]);
    let result = fetch_routes(&definition, &value, &resolved, &catalog(), &client)
        .await
        .unwrap();
    assert_eq!(
        result
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["grok-new", "grok-new@500k", "grok-new@131072"]
    );
    assert_eq!(result.models[1].context_window, 500000);
    assert_eq!(result.models[0].info.reasoning_efforts, vec!["low", "high"]);
    let requests = client.requests.lock().unwrap();
    assert_eq!(Url::parse(&requests[0].url).unwrap().path(), "/v1/models");
    assert!(requests[0].headers.contains_key("x-grok-client-version"));
    let token_auth = requests[0]
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("x-xai-token-auth"))
        .map(|(_, value)| value.as_str());
    assert_eq!(token_auth, Some("xai-grok-cli"));
    assert_eq!(
        requests[0].headers["x-grok-client-identifier"],
        "grok-shell"
    );
    assert_eq!(requests[0].headers["x-grok-client-mode"], "headless");
}

#[tokio::test]
async fn gemini_pages_use_the_native_key_header_and_content_models() {
    let definition = definition(
        "google-generative-ai",
        "https://generativelanguage.googleapis.com",
    );
    let client = client(vec![
        Reply::json(
            json!({"models":[{"name":"models/gemini-new","displayName":"Gemini New","inputTokenLimit":1048576,"outputTokenLimit":65536,"supportedGenerationMethods":["generateContent"]},{"name":"models/embedding","supportedGenerationMethods":["embedContent"]}],"nextPageToken":"page-two"}),
        ),
        Reply::json(
            json!({"models":[{"name":"models/gemini-other","supportedGenerationMethods":[]}]}),
        ),
    ]);
    let result = fetch_routes(
        &definition,
        &api_credentials(),
        &resolved(),
        &catalog(),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(
        result
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["gemini-new", "embedding", "gemini-other"]
    );
    assert_eq!(result.models[0].max_tokens, 65536);
    let requests = client.requests.lock().unwrap();
    for request in requests.iter() {
        assert_eq!(request.headers["x-goog-api-key"], "private-token");
        assert!(!request.url.contains("private-token"));
    }
    assert!(requests[1].url.contains("pageToken=page-two"));
}

#[tokio::test]
async fn copilot_only_lists_picker_models_the_account_can_use_and_keeps_protocol() {
    let mut definition = definition("openai-completions", "https://copilot.test");
    definition.config.oauth_provider = Some("github-copilot".into());
    definition.adapter = crate::test_adapter("github-copilot");
    let client = client(vec![Reply::json(
        json!({"data":[{"id":"claude-new","model_picker_enabled":true,"policy":{"state":"enabled"},"capabilities":{"type":"chat","supports":{"reasoning_effort":["low","high"]}},"supported_endpoints":["/v1/messages"]},{"id":"retired","model_picker_enabled":false,"capabilities":{"type":"chat"}},{"id":"denied","model_picker_enabled":true,"policy":{"state":"disabled"},"capabilities":{"type":"chat"}},{"id":"experimental","vendor":"Experimental","model_picker_enabled":true,"capabilities":{"type":"chat"}}]}),
    )]);
    let result = fetch_routes(
        &definition,
        &api_credentials(),
        &resolved(),
        &catalog(),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(result.models.len(), 1);
    assert_eq!(result.models[0].api, "github-copilot-anthropic-messages");
    assert_eq!(
        client.requests.lock().unwrap()[0].headers["copilot-integration-id"],
        "vscode-chat"
    );
    assert_eq!(
        client.requests.lock().unwrap()[0].headers["x-github-api-version"],
        "2026-01-09"
    );
}

#[tokio::test]
async fn copilot_uses_only_offered_http_apis_and_normalizes_their_version_prefix() {
    let mut definition = definition("openai-completions", "https://copilot.test");
    definition.config.oauth_provider = Some("github-copilot".into());
    definition.adapter = crate::test_adapter("github-copilot");
    let entries = [
        ("messages", json!(["/messages/"])),
        ("responses", json!(["/v1/responses/"])),
        ("chat", json!(["/v1/chat/completions/"])),
        ("websocket-only", json!(["ws:/responses"])),
        (
            "websocket-and-chat",
            json!(["ws:/responses", "/chat/completions"]),
        ),
        ("no-endpoints", Value::Null),
        ("empty-endpoints", json!([])),
    ]
    .into_iter()
    .map(|(id, endpoints)| {
        let mut entry = json!({"id":id,"model_picker_enabled":true,"capabilities":{"type":"chat"}});
        if !endpoints.is_null() {
            entry["supported_endpoints"] = endpoints;
        }
        entry
    })
    .collect::<Vec<_>>();
    let client = client(vec![Reply::json(json!({"data":entries}))]);
    let result = fetch_routes(
        &definition,
        &api_credentials(),
        &resolved(),
        &catalog(),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(
        result
            .models
            .iter()
            .map(|model| (model.id.as_str(), model.api.as_str()))
            .collect::<Vec<_>>(),
        [
            ("messages", "github-copilot-anthropic-messages"),
            ("responses", "github-copilot-openai-responses"),
            ("chat", "github-copilot-chat"),
            ("websocket-and-chat", "github-copilot-chat"),
            ("no-endpoints", "github-copilot-chat"),
            ("empty-endpoints", "github-copilot-chat"),
        ]
    );
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn copilot_accounts_with_no_manual_choices_still_offer_the_real_auto_route() {
    let mut definition = definition("openai-completions", "https://copilot.test");
    definition.config.oauth_provider = Some("github-copilot".into());
    definition.adapter = crate::test_adapter("github-copilot");
    let credentials =
        json!({"type":"oauth","access":"private-token","credentialProvider":"gitHubCopilot"});
    let client = client(vec![Reply::json(json!({"data":[]}))]);
    let mut remote = fetch_routes(&definition, &credentials, &resolved(), &catalog(), &client)
        .await
        .unwrap();
    supplement_models(&definition, Some(&credentials), &mut remote.models);
    assert_eq!(remote.models.len(), 1);
    assert_eq!(remote.models[0].id, "auto");
    assert_eq!(remote.models[0].api, "github-copilot-chat");
    assert!(remote.models[0].provider_options.is_empty());
    assert!(remote.failed_parts.is_empty());
    let fallback = preset(&definition, Some(&credentials), &catalog(), None).unwrap();
    assert_eq!(fallback["models"].as_array().unwrap().len(), 1);
    assert_eq!(fallback["models"][0]["id"], "auto");
}

#[tokio::test]
async fn copilot_model_reads_use_the_current_session_endpoint_instead_of_a_saved_previous_one() {
    let mut definition = definition("openai-completions", "https://saved.example.invalid");
    definition.config.oauth_provider = Some("github-copilot".into());
    definition.adapter = crate::test_adapter("github-copilot");
    let credentials = json!({"type":"oauth","access":"private-token","credentialProvider":"gitHubCopilot", "extras":{"endpoint":"https://saved.example.invalid"}});
    let mut current = resolved();
    current.base_url = Some("https://current.example.invalid".into());
    let client = client(vec![Reply::json(
        json!({"data":[{"id":"current-model","model_picker_enabled":true,"capabilities":{"type":"chat"}}]}),
    )]);
    let result = fetch_routes(&definition, &credentials, &current, &catalog(), &client)
        .await
        .unwrap();
    let requested = Url::parse(&client.requests.lock().unwrap()[0].url).unwrap();
    assert_eq!(requested.host_str(), Some("current.example.invalid"));
    assert_eq!(requested.path(), "/models");
    assert_eq!(result.models[0].base_url, "https://current.example.invalid");
    let api_bases = resolved_bases(&definition, &api_credentials(), &current);
    assert!(api_bases
        .iter()
        .all(|base| base.base_url == "https://saved.example.invalid"));
}

#[tokio::test]
async fn azure_lists_deployments_first_and_uses_api_key_alone() {
    let definition = definition(
        "azure-openai-responses",
        "https://resource.openai.azure.com",
    );
    let client = client(vec![
        Reply::status(404),
        Reply::json(json!({"data":[{"id":"my-deployment"}]})),
    ]);
    let result = fetch_routes(
        &definition,
        &api_credentials(),
        &resolved(),
        &catalog(),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(paths(&client), ["/openai/deployments", "/openai/v1/models"]);
    assert_eq!(
        result.models[0].base_url,
        "https://resource.openai.azure.com/openai/v1"
    );
    let requests = client.requests.lock().unwrap();
    assert!(requests[0].url.contains("api-version=2022-12-01"));
    for request in requests.iter() {
        assert_eq!(request.headers["api-key"], "private-token");
        assert!(!request.headers.contains_key("authorization"));
    }
}

#[tokio::test]
async fn a_connected_provider_error_returns_its_safe_fallback_and_visible_error() {
    let definition = definition("openai-completions", "https://example.test/v1");
    let client = client(vec![Reply::status(503)]);
    let directory = std::env::temp_dir().join(format!(
        "nora-model-discovery-missing-{}",
        std::process::id()
    ));
    let result = discover(&definition, Some(&api_credentials()), &directory, &client)
        .await
        .unwrap();
    assert_eq!(result["source"], "catalog");
    assert_eq!(result["models"], json!([]));
    assert!(result["modelError"].as_str().unwrap().contains("503"));
    assert!(!result.to_string().contains("private-token"));
}

#[tokio::test]
async fn explicit_models_url_is_asked_alone_keeps_inference_routes_and_never_leaks_queries() {
    let mut definition = definition("openai-completions", "https://chat.invalid/v1");
    definition.config.models_url =
        "https://catalog.invalid/private/list?secret=query-private".into();
    definition.config.model_discovery = Some(crate::definition::ModelDiscovery {
        default_base_url: definition.base_url.clone(),
        bases: vec![
            ModelBase {
                api: "openai-completions".into(),
                base_url: definition.base_url.clone(),
            },
            ModelBase {
                api: "anthropic-messages".into(),
                base_url: "https://claude.invalid".into(),
            },
        ],
        no_list: false,
    });
    let client = client(vec![Reply::json(json!({"data":[{"id":"live-model"}]}))]);
    let result = fetch_routes(
        &definition,
        &api_credentials(),
        &resolved(),
        &catalog(),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    assert_eq!(
        client.requests.lock().unwrap()[0].url,
        definition.config.models_url
    );
    assert_eq!(result.models[0].base_url, definition.base_url);
    assert!(result.failed_parts.is_empty());
    assert!(!serde_json::to_string(&result.models)
        .unwrap()
        .contains("query-private"));
    let failed = MockClient {
        requests: Mutex::new(Vec::new()),
        replies: Mutex::new(vec![Reply::status(404)].into()),
        stall_at: None,
    };
    let result = fetch_routes(
        &definition,
        &api_credentials(),
        &resolved(),
        &catalog(),
        &failed,
    )
    .await
    .err()
    .unwrap();
    assert!(!result.to_string().contains("query-private"));
    assert_eq!(failed.requests.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn model_list_custom_auth_header_and_user_headers_override_all_generated_headers() {
    let mut definition = definition("anthropic-messages", "https://relay.invalid");
    definition.config.models_url = "https://catalog.invalid/list".into();
    definition.config.api_key_auth = Some(nora_ai::AuthScheme::ApiKeyHeader {
        name: "X-Private-Key".into(),
    });
    definition
        .config
        .headers
        .insert("x-private-key".into(), "explicit-override".into());
    definition
        .config
        .headers
        .insert("Anthropic-Version".into(), "configured-version".into());
    let credentials = json!({"type":"apiKey","access":"private-token","extras":{"noraAccountHeaders":{"X-Private-Key":"account-override"}}});
    let client = client(vec![Reply::json(json!({"data":[{"id":"live-model"}]}))]);
    fetch_routes(&definition, &credentials, &resolved(), &catalog(), &client)
        .await
        .unwrap();
    let requests = client.requests.lock().unwrap();
    let header = |name: &str| {
        requests[0]
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(header("x-private-key"), Some("account-override"));
    assert_eq!(header("anthropic-version"), Some("configured-version"));
    assert_eq!(header("authorization"), None);
    assert_eq!(header("x-api-key"), None);
}

#[path = "decision_discovery.test.rs"]
mod decisions;
