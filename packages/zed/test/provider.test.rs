use super::*;
use crate::fixture::{self, Client, Reply};
fn initialize() -> InitializeParams {
    InitializeParams {
        subscription_accounts_enabled: true,
        installed_manifest: None,
        runtime_config: serde_json::Value::Null,
        api_version: API_VERSION,
        plugin_id: "zed".into(),
        plugin_directory: std::env::temp_dir(),
        data_directory: std::env::temp_dir(),
        manifest: serde_json::from_str(include_str!("../nora-provider.json")).unwrap(),
    }
}
async fn plugin(client: Arc<Client>, save: Arc<dyn Persistence>) -> Zed {
    let plugin = Zed {
        client: Some(client),
        persistence: save,
        ..Default::default()
    };
    plugin.initialize(initialize()).await.unwrap();
    plugin
}
#[derive(Default)]
struct Save {
    values: Mutex<Vec<Value>>,
    deny: std::sync::atomic::AtomicBool,
}
#[async_trait]
impl Persistence for Save {
    async fn save(&self, value: Value) -> PluginResult<()> {
        self.values.lock().await.push(value);
        if self.deny.load(std::sync::atomic::Ordering::SeqCst) {
            Err(PluginError::transport("Host declined credential storage."))
        } else {
            Ok(())
        }
    }
}
#[tokio::test]
async fn initialization_has_no_account_reads_or_network_calls() {
    let client = Client::new(vec![]);
    let plugin = plugin(client.clone(), Arc::new(Save::default())).await;
    assert_eq!(
        plugin
            .auth_methods(CallContext::default())
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        plugin.accounts(CallContext::default()).await.unwrap(),
        json!([])
    );
    assert_eq!(
        plugin.models(CallContext::default()).await.unwrap()["source"],
        "preset"
    );
    assert!(client.requests.lock().unwrap().is_empty());
    let mut wrong = initialize();
    wrong.manifest["baseURL"] = json!("https://other.test/v1");
    assert!(plugin.initialize(wrong).await.is_err());
}
#[tokio::test]
async fn live_models_repick_an_unsubscribed_organization_only_after_host_ack() {
    let client = Client::new(vec![
        Reply::json(json!({"token":"model-token-team"})),
        Reply::status(402),
        Reply::json(fixture::me()),
        Reply::json(json!({"token":"model-token-personal"})),
        Reply::json(fixture::models()),
    ]);
    let save = Arc::new(Save::default());
    let plugin = plugin(client.clone(), save.clone()).await;
    let result = plugin
        .models(CallContext {
            subscription_accounts_enabled: true,
            account: "one".into(),
            credentials: Some(fixture::saved("team")),
            network_proxy: None,
        })
        .await
        .unwrap();
    assert_eq!(result["source"], "remote");
    assert_eq!(result["models"].as_array().unwrap().len(), 4);
    assert_eq!(save.values.lock().await[0]["organizationID"], "personal");
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(requests[0].body.as_ref().unwrap()).unwrap()
            ["organization_id"],
        "team"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(requests[3].body.as_ref().unwrap()).unwrap()
            ["organization_id"],
        "personal"
    );
    assert_eq!(
        requests[1].headers["authorization"],
        "Bearer model-token-team"
    );
    assert_eq!(
        requests[2].headers["authorization"],
        "77 private-zed-access"
    );
}
#[tokio::test]
async fn rejected_credential_save_prevents_using_a_new_organization() {
    let client = Client::new(vec![
        Reply::json(json!({"token":"model-token-team"})),
        Reply::status(402),
        Reply::json(fixture::me()),
    ]);
    let save = Arc::new(Save::default());
    save.deny.store(true, std::sync::atomic::Ordering::SeqCst);
    let plugin = plugin(client.clone(), save).await;
    assert!(plugin
        .models(CallContext {
            subscription_accounts_enabled: true,
            account: "one".into(),
            credentials: Some(fixture::saved("team")),
            network_proxy: None
        })
        .await
        .is_err());
    assert_eq!(client.requests.lock().unwrap().len(), 3);
}
#[tokio::test]
async fn stale_model_token_is_minted_once_without_replacing_the_account_pair() {
    let client = Client::new(vec![
        Reply::json(json!({"token":"old-model-token"})),
        Reply::status(401),
        Reply::json(json!({"token":"new-model-token"})),
        Reply::json(fixture::models()),
    ]);
    let plugin = plugin(client.clone(), Arc::new(Save::default())).await;
    let result = plugin
        .models(CallContext {
            subscription_accounts_enabled: true,
            account: "one".into(),
            credentials: Some(fixture::saved("personal")),
            network_proxy: None,
        })
        .await
        .unwrap();
    assert_eq!(result["source"], "remote");
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        requests[0].headers["authorization"],
        requests[2].headers["authorization"]
    );
    assert_eq!(
        requests[3].headers["authorization"],
        "Bearer new-model-token"
    );
}
#[tokio::test]
async fn real_anthropic_fetch_wraps_the_native_request_and_never_forwards_caller_auth() {
    let client = Client::new(vec![
        Reply::json(json!({"token":"private-model-token"})),
        Reply::json(fixture::models()),
        Reply::lines(&fixture::anthropic(), true),
    ]);
    let plugin = plugin(client.clone(), Arc::new(Save::default())).await;
    let response=plugin.fetch(fixture::fetch("/v1/messages",json!({"model":"claude-model","stream":false,"system":"original system","messages":[{"role":"user","content":"question"}],"tools":[{"name":"original_name","input_schema":{"type":"object"}}]}))).await.unwrap();
    assert_eq!(response.status, 200);
    let body: Value = serde_json::from_str(&fixture::body(response).await).unwrap();
    assert_eq!(body["content"][0]["id"], "original-tool-id");
    let requests = client.requests.lock().unwrap();
    let request = requests.last().unwrap();
    assert_eq!(request.url, "https://cloud.zed.dev/completions");
    assert_eq!(
        request.headers["authorization"],
        "Bearer private-model-token"
    );
    assert!(!request.headers.contains_key("cookie"));
    let body: Value = serde_json::from_slice(request.body.as_ref().unwrap()).unwrap();
    assert_eq!(body["provider"], "anthropic");
    assert_eq!(body["provider_request"]["system"], "original system");
    assert!(body["provider_request"].get("stream").is_none());
}
#[tokio::test]
async fn a_native_vendor_mismatch_is_not_sent_as_another_vendors_chat() {
    let client = Client::new(vec![
        Reply::json(json!({"token":"model-token"})),
        Reply::json(fixture::models()),
    ]);
    let plugin = plugin(client.clone(), Arc::new(Save::default())).await;
    let response = plugin
        .fetch(fixture::fetch(
            "/v1/chat/completions",
            json!({"model":"claude-model","messages":[]}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status, 400);
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn usage_returns_only_published_plan_and_period_without_a_fabricated_quota() {
    let client = Client::new(vec![Reply::json(fixture::me())]);
    let plugin = plugin(client, Arc::new(Save::default())).await;
    let usage = plugin
        .usage(CallContext {
            subscription_accounts_enabled: true,
            account: "one".into(),
            credentials: Some(fixture::saved("personal")),
            network_proxy: None,
        })
        .await
        .unwrap();
    assert_eq!(usage["plan"], "Student");
    assert_eq!(usage["items"].as_array().unwrap().len(), 2);
    for item in usage["items"].as_array().unwrap() {
        assert_eq!(item["countsForRouting"], false);
        assert!(item.get("usedPercent").is_none());
        assert!(item.get("remaining").is_none());
    }
    assert!(usage["message"]
        .as_str()
        .unwrap()
        .contains("no remaining usage"));
}
#[tokio::test]
async fn each_accounts_model_token_uses_its_own_account_authorization() {
    let client = Client::new(vec![
        Reply::json(json!({"token":"one-token"})),
        Reply::json(json!({"token":"two-token"})),
    ]);
    let plugin = plugin(client.clone(), Arc::new(Save::default())).await;
    let a = Account::parse(&fixture::saved("personal")).unwrap();
    let mut second = fixture::saved("personal");
    second["userID"] = json!("88");
    second["access"] = json!("private-other-access");
    let b = Account::parse(&second).unwrap();
    assert_eq!(
        plugin.token(client.as_ref(), &a, false).await.unwrap(),
        "one-token"
    );
    assert_eq!(
        plugin.token(client.as_ref(), &b, false).await.unwrap(),
        "two-token"
    );
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        requests[0].headers["authorization"],
        "77 private-zed-access"
    );
    assert_eq!(
        requests[1].headers["authorization"],
        "88 private-other-access"
    );
}

fn model_context() -> CallContext {
    CallContext {
        subscription_accounts_enabled: true,
        account: "one".into(),
        credentials: Some(fixture::saved("personal")),
        network_proxy: None,
    }
}

#[tokio::test]
async fn model_outages_before_the_first_list_never_return_declared_models() {
    for token_failed in [true, false] {
        let mut replies = Vec::new();
        if !token_failed {
            replies.push(Reply::json(json!({"token":"model-token"})));
        }
        replies.push(Reply::status(502));
        let client = Client::new(replies);
        let plugin = plugin(client.clone(), Arc::new(Save::default())).await;
        let error = plugin.models(model_context()).await.unwrap_err();
        assert_eq!(error.code, "zed_http_502");
        assert!(plugin.models.lock().await.is_empty());
        assert_eq!(
            client.requests.lock().unwrap().len(),
            if token_failed { 1 } else { 2 }
        );
    }
}

#[tokio::test]
async fn temporary_token_or_catalog_outages_return_only_this_accounts_last_successful_list() {
    for token_failed in [true, false] {
        let client = Client::new(vec![
            Reply::json(json!({"token":"model-token"})),
            Reply::json(fixture::models()),
            Reply::status(502),
        ]);
        let plugin = plugin(client.clone(), Arc::new(Save::default())).await;
        let first = plugin.models(model_context()).await.unwrap();
        if token_failed {
            plugin.tokens.lock().await.clear();
        }
        let old_time = Instant::now() - Duration::from_secs(601);
        for (at, _) in plugin.models.lock().await.values_mut() {
            *at = old_time;
        }
        let result = plugin.models(model_context()).await.unwrap();
        assert_eq!(result["models"], first["models"]);
        assert_eq!(result["source"], "remote");
        assert_eq!(
            result["failedModelParts"],
            json!(["zed|https://cloud.zed.dev/models"])
        );
        assert_eq!(result["allowUnknownModels"], false);
        assert_eq!(client.requests.lock().unwrap().len(), 3);
        assert!(plugin
            .models
            .lock()
            .await
            .values()
            .all(|(at, _)| *at == old_time));
    }
}

#[tokio::test]
async fn account_refusal_is_reported_even_after_its_models_were_cached() {
    let client = Client::new(vec![
        Reply::json(json!({"token":"model-token"})),
        Reply::json(fixture::models()),
        Reply::status(401),
    ]);
    let plugin = plugin(client.clone(), Arc::new(Save::default())).await;
    plugin.models(model_context()).await.unwrap();
    plugin.tokens.lock().await.clear();
    let error = plugin.models(model_context()).await.unwrap_err();
    assert_eq!(error.code, "authentication_required");
    assert_eq!(client.requests.lock().unwrap().len(), 3);
    assert!(client
        .requests
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .url
        .ends_with("/client/llm_tokens"));
}

#[tokio::test]
async fn a_model_token_refused_twice_does_not_expire_the_accounts_own_sign_in() {
    let client = Client::new(vec![
        Reply::json(json!({"token":"first-model-token"})),
        Reply::status(401),
        Reply::json(json!({"token":"second-model-token"})),
        Reply::status(401),
    ]);
    let plugin = plugin(client.clone(), Arc::new(Save::default())).await;
    let error = plugin.models(model_context()).await.unwrap_err();
    assert_eq!(error.code, "zed_http_401");
    assert_ne!(error.code, "authentication_required");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests[1].headers["authorization"],
        "Bearer first-model-token"
    );
    assert_eq!(
        requests[3].headers["authorization"],
        "Bearer second-model-token"
    );
    assert_eq!(
        requests[0].headers["authorization"],
        requests[2].headers["authorization"]
    );
}

#[tokio::test]
async fn cached_models_are_never_borrowed_from_a_different_account_or_organization() {
    for changed in ["userID", "organizationID", "access"] {
        let client = Client::new(vec![
            Reply::json(json!({"token":"model-token"})),
            Reply::json(fixture::models()),
            Reply::status(502),
        ]);
        let plugin = plugin(client.clone(), Arc::new(Save::default())).await;
        plugin.models(model_context()).await.unwrap();
        let mut context = model_context();
        context.credentials.as_mut().unwrap()[changed] = json!(if changed == "userID" {
            "88"
        } else {
            "different"
        });
        assert_eq!(
            plugin.models(context).await.unwrap_err().code,
            "zed_http_502"
        );
        assert_eq!(client.requests.lock().unwrap().len(), 3);
    }
}
