use super::*;
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine};
use nora_ai::{HttpClientError, HttpStreamingResponse};
use std::{collections::VecDeque, path::PathBuf, sync::Mutex as StdMutex};

struct Client {
    requests: StdMutex<Vec<HttpRequest>>,
    replies: StdMutex<VecDeque<(u16, Value)>>,
}
impl Client {
    fn new(replies: Vec<(u16, Value)>) -> Arc<Self> {
        Arc::new(Self {
            requests: StdMutex::default(),
            replies: StdMutex::new(replies.into()),
        })
    }
}
#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        let (status, value) = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected Bedrock HTTP request");
        Ok(HttpStreamingResponse {
            status,
            headers: BTreeMap::new(),
            body: Box::pin(futures::stream::once(async move {
                Ok(value.to_string().into_bytes().into())
            })),
        })
    }
}
struct Store;
#[async_trait]
impl core::CredentialStore for Store {
    async fn save(&self, _: Value) -> PluginResult<()> {
        panic!("Bedrock keys must never rotate implicitly")
    }
}
async fn configured(mantle: bool, client: Arc<Client>) -> Bedrock {
    let provider = Bedrock::with_dependencies(mantle, client, Arc::new(Store));
    provider
        .initialize(InitializeParams {
            subscription_accounts_enabled: true,
            installed_manifest: None,
            runtime_config: serde_json::Value::Null,
            api_version: 1,
            plugin_id: provider.core.id.into(),
            plugin_directory: PathBuf::from("/tmp/bedrock"),
            data_directory: PathBuf::from("/tmp/bedrock"),
            manifest: serde_json::from_str(if mantle {
                include_str!("../../amazon-bedrock-mantle/nora-provider.json")
            } else {
                include_str!("../nora-provider.json")
            })
            .unwrap(),
        })
        .await
        .unwrap();
    provider
}
fn context() -> CallContext {
    CallContext {
        subscription_accounts_enabled: true,
        credentials: Some(json!({"key":"ABSK-own-key","region":"us-east-1"})),
        ..Default::default()
    }
}
fn fetch(mantle: bool, model: &str) -> FetchRequest {
    serde_json::from_value(json!({"url":if mantle{"https://bedrock-mantle.us-east-1.api.aws/v1/chat/completions"}else{"https://bedrock-runtime.us-east-1.amazonaws.com/anthropic/v1/messages"},"method":"POST","credentials":context().credentials,"headers":{"anthropic-beta":"brand-new-2026-09-01,context-1m-2025-08-07"},"body":STANDARD.encode(json!({"model":model,"messages":[{"role":"user","content":"hi"}]}).to_string())})).unwrap()
}
#[tokio::test]
async fn runtime_returns_an_explicit_preset_without_contacting_a_fake_models_endpoint() {
    let client = Client::new(vec![]);
    let provider = configured(false, client.clone()).await;
    let listing = provider.models(context()).await.unwrap();
    assert_eq!(listing["source"], "preset");
    let models = listing["models"].as_array().unwrap();
    assert!(models.len() >= 18);
    assert_eq!(
        models
            .iter()
            .find(|model| model["id"] == "global.openai.gpt-6-luna")
            .unwrap()["api"],
        "openai-responses"
    );
    assert!(models
        .iter()
        .filter(|model| model["id"].as_str().unwrap().contains("anthropic.claude"))
        .all(|model| model["api"] == "anthropic-messages"));
    assert!(client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn bearer_templates_distinguish_messages_responses_and_open_weight_chat() {
    let client = Client::new(vec![]);
    let provider = configured(false, client).await;
    let listing = provider.models(context()).await.unwrap();
    let templates = listing["modelTemplates"].as_array().unwrap();
    for (pattern, sample, api, path) in [
        (
            "*anthropic.claude*",
            "arn:aws:bedrock:us-east-1:123:inference-profile/global.anthropic.claude-future",
            "anthropic-messages",
            "/anthropic",
        ),
        (
            "*openai.gpt-oss*",
            "eu.openai.gpt-oss-120b",
            "openai-completions",
            "/openai/v1",
        ),
        (
            "*openai.gpt-*",
            "global.openai.gpt-6-luna",
            "openai-responses",
            "/openai/v1",
        ),
        ("*", "unknown.manual", "openai-completions", "/openai/v1"),
    ] {
        let model = &templates
            .iter()
            .find(|template| template["pattern"] == pattern)
            .unwrap()["model"];
        assert_eq!(model["api"], api);
        assert_eq!(
            model["baseUrl"],
            format!("https://bedrock-runtime.us-east-1.amazonaws.com{path}")
        );
        assert_eq!(model["contextWindow"], 0);
        assert_eq!(model["maxTokens"], 0);
        assert_eq!(model["info"]["pricingKnown"], false);
        let account =
            Account::saved(context().credentials.as_ref().unwrap(), &json!({}), false).unwrap();
        assert_eq!(request::route(sample, &account, false).0, api);
    }
    assert!(!listing["modelTemplates"]
        .to_string()
        .contains("ABSK-own-key"));
}

#[tokio::test]
async fn aws_templates_and_discovered_models_keep_the_account_region_and_converse_protocol() {
    let provider = configured(false, Client::new(vec![])).await;
    let context = CallContext {
        subscription_accounts_enabled: true,
        credentials: Some(
            json!({"access":json!({"accessKeyId":"AKIA-own","secretAccessKey":"private-secret"}).to_string(),"region":"eu-west-1"}),
        ),
        ..Default::default()
    };
    let listing = provider.models(context).await.unwrap();
    assert_eq!(listing["defaultAPI"], "bedrock-converse-stream");
    assert_eq!(
        listing["defaultBaseURL"],
        "https://bedrock-runtime.eu-west-1.amazonaws.com"
    );
    let templates = listing["modelTemplates"].as_array().unwrap();
    assert_eq!(templates.len(), 1);
    assert_eq!(templates[0]["pattern"], "*");
    assert_eq!(templates[0]["model"]["api"], "bedrock-converse-stream");
    assert_eq!(
        templates[0]["model"]["providerOptions"]["region"],
        "eu-west-1"
    );
    assert!(listing["models"]
        .as_array()
        .unwrap()
        .iter()
        .all(|model| model["api"] == "bedrock-converse-stream"
            && model["providerOptions"]["region"] == "eu-west-1"));
    assert!(!listing.to_string().contains("private-secret"));
}

#[tokio::test]
async fn mantle_templates_keep_native_prefixes_and_unknown_limits_remain_unknown() {
    let client = Client::new(vec![(
        200,
        json!({"data":[{"id":"newmaker.manual"}],"has_more":false}),
    )]);
    let provider = configured(true, client).await;
    let listing = provider.models(context()).await.unwrap();
    assert_eq!(listing["source"], "remote");
    assert_eq!(listing["models"][0]["contextWindow"], 0);
    assert_eq!(listing["models"][0]["maxTokens"], 0);
    assert_eq!(listing["models"][0]["info"]["pricingKnown"], false);
    for (pattern, path) in [
        ("*", "/v1"),
        ("xai.*", "/openai/v1"),
        ("*openai.gpt-*", "/openai/v1"),
        ("*openai.gpt-oss*", "/v1"),
    ] {
        let template = listing["modelTemplates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|template| template["pattern"] == pattern)
            .unwrap();
        assert_eq!(
            template["model"]["baseUrl"],
            format!("https://bedrock-mantle.us-east-1.api.aws{path}")
        );
    }
}

#[tokio::test]
async fn unconnected_mantle_does_not_claim_remote_discovery_or_make_a_request() {
    let client = Client::new(vec![]);
    let provider = configured(true, client.clone()).await;
    let listing = provider.models(CallContext::default()).await.unwrap();
    assert_eq!(listing["source"], "preset");
    assert_eq!(listing["models"], json!([]));
    assert!(!listing["modelTemplates"].as_array().unwrap().is_empty());
    assert!(client.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn mantle_models_are_account_live_and_empty_results_do_not_restore_presets() {
    let client = Client::new(vec![
        (
            200,
            json!({"data":[{"id":"openai.gpt-5.5","context_length":256000},{"id":"anthropic.claude-opus-4-7"}],"has_more":true,"last_id":"page-one"}),
        ),
        (
            200,
            json!({"data":[{"id":"google.gemma-4"}],"has_more":false}),
        ),
        (200, json!({"data":[]})),
    ]);
    let provider = configured(true, client.clone()).await;
    let first = provider.models(context()).await.unwrap();
    assert_eq!(first["source"], "remote");
    assert_eq!(first["models"].as_array().unwrap().len(), 3);
    assert_eq!(first["models"][0]["contextWindow"], 256000);
    let sent = client.requests.lock().unwrap();
    assert!(sent[0].url.ends_with("/v1/models"));
    assert!(sent[1].url.ends_with("?after=page-one"));
    assert_eq!(sent[0].headers["authorization"], "Bearer ABSK-own-key");
    drop(sent);
    assert_eq!(
        provider.models(context()).await.unwrap()["models"],
        json!([])
    );
}
#[tokio::test]
async fn mantle_wrong_prefix_is_retried_once_and_learned_only_for_that_account_model() {
    let client = Client::new(vec![
        (
            400,
            json!({"message":"model `newmaker.model` isn't supported on this route"}),
        ),
        (200, json!({"choices":[]})),
        (200, json!({"choices":[]})),
    ]);
    let provider = configured(true, client.clone()).await;
    assert_eq!(
        provider
            .fetch(fetch(true, "newmaker.model"))
            .await
            .unwrap()
            .status,
        200
    );
    assert_eq!(
        provider
            .fetch(fetch(true, "newmaker.model"))
            .await
            .unwrap()
            .status,
        200
    );
    let sent = client.requests.lock().unwrap();
    assert!(sent[0].url.ends_with("/v1/chat/completions"));
    assert!(sent[1].url.ends_with("/openai/v1/chat/completions"));
    assert_eq!(sent[1].url, sent[2].url);
}
#[tokio::test]
async fn unknown_anthropic_beta_is_removed_only_after_its_explicit_refusal() {
    let client = Client::new(vec![
        (
            400,
            json!({"message":"Unexpected value(s) `brand-new-2026-09-01` for the `anthropic-beta` header"}),
        ),
        (200, json!({"content":[]})),
        (200, json!({"content":[]})),
    ]);
    let provider = configured(false, client.clone()).await;
    assert_eq!(
        provider
            .fetch(fetch(false, "global.anthropic.claude-opus-5-5"))
            .await
            .unwrap()
            .status,
        200
    );
    assert_eq!(
        provider
            .fetch(fetch(false, "global.anthropic.claude-opus-5-5"))
            .await
            .unwrap()
            .status,
        200
    );
    let sent = client.requests.lock().unwrap();
    assert!(sent[0].headers["anthropic-beta"].contains("brand-new"));
    assert_eq!(sent[1].headers["anthropic-beta"], "context-1m-2025-08-07");
    assert_eq!(
        sent[1].headers["anthropic-beta"],
        sent[2].headers["anthropic-beta"]
    );
}
#[tokio::test]
async fn rejected_bodies_redact_account_keys_without_retrying_visible_success() {
    use futures::StreamExt;
    let client = Client::new(vec![(400, json!({"message":"invalid key ABSK-own-key"}))]);
    let provider = configured(false, client.clone()).await;
    let mut response = provider
        .fetch(fetch(false, "global.anthropic.claude-opus-5-5"))
        .await
        .unwrap();
    assert_eq!(response.status, 400);
    let bytes = response.body.next().await.unwrap().unwrap();
    assert!(!String::from_utf8(bytes).unwrap().contains("ABSK-own-key"));
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn initialization_and_usage_do_not_discover_or_import_an_account() {
    let client = Client::new(vec![]);
    let provider = configured(true, client.clone()).await;
    assert_eq!(
        provider.accounts(CallContext::default()).await.unwrap(),
        json!([])
    );
    assert_eq!(provider.usage(context()).await.unwrap()["available"], false);
    assert!(client.requests.lock().unwrap().is_empty());
}
