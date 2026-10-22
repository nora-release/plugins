use super::*;
use futures::StreamExt;
#[path = "fixture.rs"]
mod fixture;
async fn provider(app: App, replies: Vec<fixture::Reply>) -> (CodeAssist, Arc<fixture::Client>) {
    let client = Arc::new(fixture::Client::new(replies));
    let mut provider = CodeAssist::new(app);
    provider.client = Some(client.clone());
    let manifest: Value = serde_json::from_str(match app {
        App::GeminiCLI => include_str!("../../gemini-cli/nora-provider.json"),
        App::Antigravity => include_str!("../../antigravity/nora-provider.json"),
    })
    .unwrap();
    provider
        .initialize(InitializeParams {
            api_version: API_VERSION,
            plugin_id: app.id().into(),
            plugin_directory: Default::default(),
            data_directory: Default::default(),
            manifest,
        })
        .await
        .unwrap();
    (provider, client)
}
fn credentials(app: App) -> Value {
    json!({"type":"oauth","app":app.id(),"access":"private-access","refresh":"private-refresh","expires":now()+3600000,"accountId":"bound-person","email":"person@example.test","project":"project-a"})
}
fn context(app: App) -> CallContext {
    CallContext {
        account: "work".into(),
        credentials: Some(credentials(app)),
        network_proxy: None,
    }
}
fn project() -> fixture::Reply {
    fixture::Reply::json(
        json!({"currentTier":{"id":"free-tier","name":"Free"},"cloudaicompanionProject":"project-a"}),
    )
}
fn request(app: App, model: &str) -> FetchRequest {
    use base64::Engine;
    FetchRequest {
        account: "work".into(),
        credentials: Some(credentials(app)),
        network_proxy: None,
        url: format!(
            "{}/v1beta/models/{model}:streamGenerateContent?alt=sse&key=caller-secret",
            app.base()
        ),
        method: "POST".into(),
        headers: BTreeMap::from([("authorization".into(), "caller-secret".into())]),
        metadata: BTreeMap::new(),
        body: Some(
            base64::engine::general_purpose::STANDARD
                .encode(br#"{"contents":[{"role":"user","parts":[{"text":"hello"}]}]}"#),
        ),
        body_file: None,
    }
}
#[tokio::test]
async fn gemini_models_usage_and_chat_use_real_code_assist_routes_and_model_mapping() {
    let(provider,client)=provider(App::GeminiCLI,vec![project(),fixture::Reply::json(json!({"buckets":[{"modelId":"gemini-3-flash","remainingFraction":0.8}]})),fixture::Reply::json(json!({"flags":[{"flagId":45842815,"boolValue":true}]})),fixture::Reply::json(json!({"buckets":[{"modelId":"gemini-3-flash","remainingFraction":0.8}]})),fixture::Reply::raw(200,b"data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hello\"}]},\"finishReason\":\"STOP\"}]}}\n\n")]).await;
    let models = provider.models(context(App::GeminiCLI)).await.unwrap();
    assert!(models["models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|model| model["id"] == "gemini-3.8-flash"));
    let usage = provider.usage(context(App::GeminiCLI)).await.unwrap();
    assert_eq!(usage["items"][0]["modelIDs"], json!(["gemini-3.8-flash"]));
    let response = provider
        .fetch(request(App::GeminiCLI, "gemini-3.5-flash"))
        .await
        .unwrap();
    let body = response
        .body
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .concat();
    assert!(String::from_utf8(body).unwrap().contains("hello"));
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        requests.last().unwrap().url,
        format!("{PROD}/v1internal:streamGenerateContent?alt=sse")
    );
    assert!(requests
        .iter()
        .all(|request| !request.url.contains("caller-secret")));
    let sent: Value =
        serde_json::from_slice(requests.last().unwrap().body.as_ref().unwrap()).unwrap();
    assert_eq!(sent["model"], "gemini-3.8-flash");
    assert_eq!(sent["project"], "project-a");
    assert_eq!(
        requests.last().unwrap().headers["authorization"],
        "Bearer private-access"
    );
}
#[tokio::test]
async fn antigravity_paid_entitlement_falls_back_without_changing_the_project_or_app() {
    let models = json!({"models":{"claude-x":{"displayName":"Claude X","quotaInfo":{"remainingFraction":0.5}}}});
    let(provider,client)=provider(App::Antigravity,vec![fixture::Reply::json(json!({"currentTier":{"id":"standard-tier"},"paidTier":{"id":"g1-pro-tier"},"cloudaicompanionProject":"project-a"})),fixture::Reply::raw(400,b"private-access"),fixture::Reply::json(models),fixture::Reply::json(json!({"groups":[]}))]).await;
    let usage = provider.usage(context(App::Antigravity)).await.unwrap();
    assert_eq!(usage["items"][0]["usedPercent"], 50.0);
    let captured = client.requests.lock().unwrap();
    let requests = captured
        .iter()
        .filter(|request| request.url != VERSION)
        .collect::<Vec<_>>();
    assert!(requests[0].url.starts_with(PROD));
    assert!(requests[1..]
        .iter()
        .all(|request| request.url.starts_with(App::Antigravity.base())));
    let bodies = requests[1..3]
        .iter()
        .map(|request| serde_json::from_slice::<Value>(request.body.as_ref().unwrap()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(bodies[0]["entitlement"]["userTier"], "g1-pro-tier");
    assert_eq!(bodies[1]["entitlement"]["userTier"], "standard-tier");
}
#[tokio::test]
async fn foreign_endpoints_and_invalid_network_settings_fail_before_any_request() {
    let (provider, client) = provider(App::GeminiCLI, vec![]).await;
    let mut foreign = request(App::GeminiCLI, "gemini-3.5-flash");
    foreign.url = "https://unrelated.invalid/v1beta/models/gemini:streamGenerateContent".into();
    assert!(provider.fetch(foreign).await.is_err());
    let mut proxied = request(App::GeminiCLI, "gemini-3.5-flash");
    proxied.network_proxy = Some(NetworkProxy::Proxy {
        url: "ftp://private-user:private-password@proxy.invalid".into(),
    });
    let error = provider.fetch(proxied).await.err().unwrap();
    assert!(!error.message.contains("private-password"));
    assert!(client.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn selected_source_identity_changes_fail_without_writing_or_saving_its_tokens() {
    let path = std::env::temp_dir().join(format!(
        "nora-codeassist-bound-{}.json",
        nora_ai::oauth_pkce::random_hex(8)
    ));
    let original = serde_json::to_vec(&json!({"refresh_token":"source-refresh"})).unwrap();
    std::fs::write(&path, &original).unwrap();
    let (provider, client) = provider(
        App::GeminiCLI,
        vec![
            fixture::Reply::json(json!({"access_token":"source-access","expires_in":3600})),
            fixture::Reply::json(json!({"id":"person-a","email":"a@example.test"})),
            project(),
            fixture::Reply::json(json!({"access_token":"changed-access","expires_in":3600})),
            fixture::Reply::json(json!({"id":"person-b","email":"b@example.test"})),
        ],
    )
    .await;
    let authorized = provider
        .authorize(json!({"method":"source","inputs":{"sourcePath":path}}))
        .await
        .unwrap();
    let saved = &authorized["credentials"];
    assert!(!saved.to_string().contains("source-refresh"));
    assert!(!saved.to_string().contains("source-access"));
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let error = provider
        .models(CallContext {
            account: "work".into(),
            credentials: Some(saved.clone()),
            network_proxy: None,
        })
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, "authentication_required");
    assert_eq!(client.requests.lock().unwrap().len(), 5);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    std::fs::remove_file(path).unwrap();
}
#[tokio::test]
async fn body_files_are_verified_and_transformed_through_the_same_chat_contract() {
    let (provider, client) = provider(
        App::GeminiCLI,
        vec![
            project(),
            fixture::Reply::json(json!({"flags":[]})),
            fixture::Reply::raw(
                200,
                b"data: {\"response\":{\"usageMetadata\":{\"promptTokenCount\":1}}}\n\n",
            ),
        ],
    )
    .await;
    let path = std::env::temp_dir().join(format!(
        "nora-codeassist-body-{}.json",
        nora_ai::oauth_pkce::random_hex(8)
    ));
    std::fs::write(&path,br#"{"contents":[{"role":"user","parts":[{"inlineData":{"mimeType":"image/png","data":"aW1hZ2U="}}]}]}"#).unwrap();
    let mut incoming = request(App::GeminiCLI, "gemini-3.5-flash");
    incoming.body = None;
    incoming.body_file = Some(nora_provider_plugin_sdk::RequestBodyFile::from_path(&path).unwrap());
    let response = provider.fetch(incoming).await.unwrap();
    let _ = response.body.collect::<Vec<_>>().await;
    let requests = client.requests.lock().unwrap();
    let body: Value =
        serde_json::from_slice(requests.last().unwrap().body.as_ref().unwrap()).unwrap();
    assert_eq!(
        body["request"]["contents"][0]["parts"][0]["inlineData"]["data"],
        "aW1hZ2U="
    );
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn startup_and_unconnected_model_queries_do_not_find_or_import_accounts() {
    for app in [App::GeminiCLI, App::Antigravity] {
        let (provider, client) = provider(app, vec![]).await;
        let result = provider.models(CallContext::default()).await.unwrap();
        assert!(result["models"].as_array().unwrap().is_empty());
        assert_eq!(
            provider.accounts(CallContext::default()).await.unwrap(),
            json!([])
        );
        assert!(client.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn antigravity_version_is_read_without_account_tokens_and_cached_for_six_hours() {
    let (provider, client) = provider(App::Antigravity, vec![]).await;
    assert_eq!(provider.client_version(client.as_ref()).await, "3.0.1");
    assert_eq!(provider.client_version(client.as_ref()).await, "3.0.1");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, VERSION);
    assert!(requests[0]
        .headers
        .keys()
        .all(|name| !name.eq_ignore_ascii_case("authorization")));
}

#[tokio::test]
async fn private_effort_metadata_selects_the_real_antigravity_variant_without_becoming_http_data() {
    let catalog = json!({"models":{"gemini-3.8-flash-low":{"displayName":"Gemini 3.8 Flash (Low)"},"gemini-3.8-flash-high":{"displayName":"Gemini 3.8 Flash (High)"}}});
    let (provider, client) = provider(
        App::Antigravity,
        vec![
            project(),
            fixture::Reply::json(catalog),
            fixture::Reply::raw(
                200,
                b"data: {\"response\":{\"usageMetadata\":{\"promptTokenCount\":1}}}\n\n",
            ),
        ],
    )
    .await;
    let mut incoming = request(App::Antigravity, "gemini-3.8-flash");
    incoming
        .metadata
        .insert("reasoningEffort".into(), "low".into());
    let response = provider.fetch(incoming).await.unwrap();
    let _ = response.body.collect::<Vec<_>>().await;
    let requests = client.requests.lock().unwrap();
    let last = requests.last().unwrap();
    let body: Value = serde_json::from_slice(last.body.as_ref().unwrap()).unwrap();
    assert_eq!(body["model"], "gemini-3.8-flash-low");
    assert_eq!(
        body["request"]["generationConfig"]["thinkingConfig"]["thinkingLevel"],
        "low"
    );
    assert!(!String::from_utf8_lossy(last.body.as_ref().unwrap()).contains("reasoningEffort"));
    assert!(last
        .headers
        .keys()
        .all(|key| !key.eq_ignore_ascii_case("reasoningEffort")));
}
