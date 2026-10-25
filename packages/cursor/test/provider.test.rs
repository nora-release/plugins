use super::*;
use crate::fixture::{self, Client, Reply};
fn initialize() -> InitializeParams {
    InitializeParams {
        subscription_accounts_enabled: true,
        installed_manifest: None,
        runtime_config: serde_json::Value::Null,
        api_version: API_VERSION,
        plugin_id: "cursor".into(),
        plugin_directory: std::env::temp_dir(),
        data_directory: std::env::temp_dir(),
        manifest: serde_json::from_str(include_str!("../nora-provider.json")).unwrap(),
    }
}
async fn plugin(client: Arc<Client>) -> Cursor {
    let plugin = Cursor {
        client: Some(client),
        ..Default::default()
    };
    plugin.initialize(initialize()).await.unwrap();
    plugin
}
#[tokio::test]
async fn initialization_is_offline_does_not_import_accounts_and_requires_its_own_manifest() {
    let client = Client::new(vec![]);
    let plugin = plugin(client.clone()).await;
    assert!(client.requests.lock().unwrap().is_empty());
    assert_eq!(
        plugin.accounts(CallContext::default()).await.unwrap(),
        json!([])
    );
    let models = plugin.models(CallContext::default()).await.unwrap();
    assert_eq!(models["models"], json!([]));
    assert_eq!(models["source"], "preset");
    assert_eq!(
        plugin
            .auth_methods(CallContext::default())
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let mut wrong = initialize();
    wrong.plugin_id = "other".into();
    assert!(plugin.initialize(wrong).await.is_err());
}
#[tokio::test]
async fn api_key_auth_uses_the_official_exchange_and_real_identity() {
    let client = Client::new(vec![
        Reply::json(json!({"accessToken":"private-exchanged-token"})),
        Reply::json(json!({"email":"verified@example.test"})),
        Reply::json(json!({"planInfo":{"planName":"Pro+"}})),
    ]);
    let plugin = plugin(client.clone()).await;
    let result = plugin
        .authorize(json!({"authMethodID":"apiKey","inputs":{"apiKey":"private-api-key"}}))
        .await
        .unwrap();
    assert_eq!(result["credentials"]["accountId"], "verified@example.test");
    assert_eq!(result["credentials"]["access"], "private-exchanged-token");
    let requests = client.requests.lock().unwrap();
    assert!(requests[0].url.ends_with("/auth/exchange_user_api_key"));
    assert_eq!(
        requests[0].headers["authorization"],
        "Bearer private-api-key"
    );
    assert_eq!(
        requests[1].headers["authorization"],
        "Bearer private-exchanged-token"
    );
    assert!(requests.iter().all(|r| r.url.starts_with(network::API)));
}
#[tokio::test]
async fn selected_source_is_re_read_without_copying_tokens_or_crossing_accounts() {
    let client = Client::new(vec![
        Reply::json(json!({"email":"selected@example.test"})),
        Reply::json(json!({"planInfo":{"planName":"Pro"}})),
        Reply::json(json!({"email":"different@example.test"})),
        Reply::json(json!({"planInfo":{"planName":"Ultra"}})),
    ]);
    let plugin = plugin(client.clone()).await;
    let path = std::env::temp_dir().join(format!("cursor-source-{}", auth::random()));
    let original = json!({"accessToken":"private-selected-token"}).to_string();
    std::fs::write(&path, &original).unwrap();
    let result = plugin
        .authorize(json!({"authMethodID":"cliSource","inputs":{"sourcePath":path}}))
        .await
        .unwrap();
    assert!(result["credentials"]["access"].is_null());
    assert!(!result["credentials"]
        .to_string()
        .contains("private-selected-token"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    std::fs::write(
        &path,
        json!({"accessToken":"private-replaced-token"}).to_string(),
    )
    .unwrap();
    let error = plugin
        .live(Some(&result["credentials"]), client.as_ref(), false)
        .await
        .unwrap_err();
    assert_eq!(error.code, "authentication_required");
    std::fs::remove_file(path).unwrap();
}
#[derive(Default)]
struct Save {
    accepted: std::sync::atomic::AtomicBool,
    values: Mutex<Vec<Value>>,
}
#[async_trait]
impl Persistence for Save {
    async fn save(&self, value: Value) -> PluginResult<()> {
        self.values.lock().await.push(value);
        if self.accepted.load(std::sync::atomic::Ordering::SeqCst) {
            Ok(())
        } else {
            Err(PluginError::transport(
                "The host did not acknowledge credential storage.",
            ))
        }
    }
}
#[tokio::test]
async fn rotated_api_key_access_is_unusable_until_the_host_acknowledges_storage() {
    let client = Client::new(vec![
        Reply::json(json!({"accessToken":"new-private-access"})),
        Reply::json(json!({"email":"one@example.test"})),
        Reply::json(json!({"planInfo":{"planName":"Pro"}})),
    ]);
    let save = Arc::new(Save::default());
    let plugin = Cursor {
        client: Some(client.clone()),
        persistence: save.clone(),
        ..Default::default()
    };
    plugin.initialize(initialize()).await.unwrap();
    let mut credential = fixture::credential();
    credential["authMethod"] = json!("apiKey");
    credential["key"] = json!("private-api-key");
    credential["expires"] = json!(1);
    assert!(plugin
        .live(Some(&credential), client.as_ref(), false)
        .await
        .is_err());
    assert_eq!(client.requests.lock().unwrap().len(), 3);
    save.accepted
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let next = plugin
        .live(Some(&credential), client.as_ref(), false)
        .await
        .unwrap();
    assert_eq!(next["access"], "new-private-access");
    assert_eq!(client.requests.lock().unwrap().len(), 3);
    assert_eq!(save.values.lock().await.len(), 2);
}
#[tokio::test]
async fn expired_browser_credentials_do_not_invent_a_refresh_endpoint() {
    let client = Client::new(vec![]);
    let plugin = plugin(client.clone()).await;
    let mut c = fixture::credential();
    c["expires"] = json!(1);
    assert_eq!(
        plugin
            .live(Some(&c), client.as_ref(), false)
            .await
            .unwrap_err()
            .code,
        "authentication_required"
    );
    assert!(client.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn model_discovery_is_remote_and_respects_the_account_list() {
    let client = Client::new(vec![
        Reply::json(fixture::picker()),
        Reply::json(json!({"models":[{"modelId":"claude-medium"}]})),
    ]);
    let plugin = plugin(client.clone()).await;
    let result = plugin
        .models(CallContext {
            subscription_accounts_enabled: true,
            account: "one".into(),
            credentials: Some(fixture::credential()),
            network_proxy: None,
        })
        .await
        .unwrap();
    assert_eq!(result["source"], "remote");
    assert_eq!(result["models"].as_array().unwrap().len(), 1);
    assert_eq!(result["models"][0]["id"], "claude@200k");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].headers["x-ghost-mode"], "true");
    assert_eq!(
        serde_json::from_slice::<Value>(requests[0].body.as_ref().unwrap()).unwrap()
            ["useModelParameters"],
        true
    );
}
#[tokio::test]
async fn custom_urls_sampling_and_invalid_proxy_are_rejected_before_any_key_leaves() {
    use base64::Engine;
    let client = Client::new(vec![]);
    let plugin = plugin(client.clone()).await;
    let request = |url: &str, body: Value| FetchRequest {
        subscription_accounts_enabled: true,
        account: "one".into(),
        credentials: Some(fixture::credential()),
        network_proxy: None,
        url: url.into(),
        method: "POST".into(),
        headers: BTreeMap::from([("authorization".into(), "Bearer injected".into())]),
        metadata: BTreeMap::new(),
        body: Some(base64::engine::general_purpose::STANDARD.encode(body.to_string())),
        body_file: None,
    };
    assert!(plugin
        .fetch(request(
            "https://other.test/chat/completions",
            fixture::chat()
        ))
        .await
        .is_err());
    let mut chat = fixture::chat();
    chat["temperature"] = json!(0.5);
    assert!(plugin
        .fetch(request(
            &format!("{}/chat/completions", network::BASE),
            chat
        ))
        .await
        .is_err());
    assert!(plugin.authorize(json!({"authMethodID":"apiKey","inputs":{"apiKey":"secret"},"networkProxy":{"mode":"proxy","url":"http://bad:invalid"}})).await.is_err());
    assert!(client.requests.lock().unwrap().is_empty());
}
#[test]
fn provider_http_errors_preserve_status_without_reflecting_bodies() {
    for (code, status) in [
        ("resource_exhausted", 429),
        ("permission_denied", 403),
        ("unauthenticated", 401),
        ("invalid_argument", 400),
    ] {
        let error = network::failure(
            502,
            &json!({"code":code,"message":"private-token secret-prompt"}),
        );
        assert!(!error.message.contains("private"));
        assert_eq!(stream::failure(error).status, status);
    }
}

fn fetch_chat(chat: Value) -> FetchRequest {
    use base64::Engine;
    FetchRequest {
        subscription_accounts_enabled: true,
        account: "one".into(),
        credentials: Some(fixture::credential()),
        network_proxy: None,
        url: format!("{}/chat/completions", network::BASE),
        method: "POST".into(),
        headers: BTreeMap::new(),
        metadata: BTreeMap::from([("sessionID".into(), "fixture-session".into())]),
        body: Some(base64::engine::general_purpose::STANDARD.encode(chat.to_string())),
        body_file: None,
    }
}
fn finished() -> Reply {
    let mut answer = fixture::update(1, crate::protobuf::Pb::new().str(1, "Regional answer."));
    answer.extend(fixture::update(
        14,
        crate::protobuf::Pb::new().num(1, 20).num(2, 4),
    ));
    Reply {
        status: 200,
        chunks: vec![answer],
    }
}
fn region_config(host: &str) -> Reply {
    Reply::json(json!({"agentUrlConfig":{"agentUrl":host}}))
}

#[tokio::test]
async fn a_region_refusal_refreshes_server_configuration_and_retries_once_before_output() {
    for repeated in [false, true] {
        let region = || {
            Reply::status(
                403,
                json!({"code":"permission_denied","message":"This team is served in its region only"}),
            )
        };
        let mut replies = vec![
            Reply::json(fixture::picker()),
            Reply::json(json!({"models":[{"modelId":"claude-medium"}]})),
            region_config("https://old.cursor.sh"),
            region(),
            region_config("https://eu.cursor.sh"),
        ];
        if repeated {
            replies.push(region());
        } else {
            replies.extend([finished(), Reply::json(json!({}))]);
        }
        let client = Client::new(replies);
        let plugin = plugin(client.clone()).await;
        let mut chat = fixture::chat();
        chat["stream"] = json!(false);
        let response = plugin.fetch(fetch_chat(chat)).await.unwrap();
        assert_eq!(response.status, if repeated { 403 } else { 200 });
        let body = fixture::body(response).await;
        if !repeated {
            assert!(body.contains("Regional answer."));
        }
        let requests = client.requests.lock().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.url.ends_with("/GetServerConfig"))
                .count(),
            2
        );
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.url.ends_with("/RunSSE"))
                .count(),
            2
        );
        assert!(!requests.iter().any(|r| r.url.contains("/auth/")));
    }
}

#[tokio::test]
async fn an_ordinary_permission_refusal_is_returned_without_region_retry() {
    let client = Client::new(vec![
        Reply::json(fixture::picker()),
        Reply::json(json!({"models":[{"modelId":"claude-medium"}]})),
        region_config("https://eu.cursor.sh"),
        Reply::status(
            403,
            json!({"code":"permission_denied","message":"Plan does not allow this model"}),
        ),
    ]);
    let plugin = plugin(client.clone()).await;
    assert_eq!(
        plugin
            .fetch(fetch_chat(fixture::chat()))
            .await
            .unwrap()
            .status,
        403
    );
    assert_eq!(client.requests.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn a_region_error_after_output_never_replays_the_request() {
    let mut bytes = fixture::update(1, crate::protobuf::Pb::new().str(1, "Already started."));
    let mut end = crate::protobuf::frame(
        br#"{"error":{"code":"permission_denied","message":"region refused"}}"#,
    );
    end[0] = 2;
    bytes.extend(end);
    let client = Client::new(vec![
        Reply::json(fixture::picker()),
        Reply::json(json!({"models":[{"modelId":"claude-medium"}]})),
        region_config("https://eu.cursor.sh"),
        Reply {
            status: 200,
            chunks: vec![bytes],
        },
        Reply::json(json!({})),
    ]);
    let plugin = plugin(client.clone()).await;
    assert_eq!(
        plugin
            .fetch(fetch_chat(fixture::chat()))
            .await
            .unwrap()
            .status,
        502
    );
    assert_eq!(client.requests.lock().unwrap().len(), 5);
}
