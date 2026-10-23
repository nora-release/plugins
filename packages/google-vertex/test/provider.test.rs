use super::*;
#[path = "support.rs"]
mod support;
use base64::{engine::general_purpose::STANDARD, Engine};
use nora_provider_plugin_sdk::ProviderPlugin;

fn saved(config: &Config) -> Value {
    serde_json::to_value(config).unwrap()
}
fn context(config: &Config) -> CallContext {
    CallContext {
        credentials: Some(saved(config)),
        ..Default::default()
    }
}
fn request(config: &Config) -> FetchRequest {
    FetchRequest {
        account: "one".into(),
        credentials: Some(saved(config)),
        network_proxy: None,
        url: format!(
            "{}/models/gemini-live:streamGenerateContent?alt=sse&key=nora-plugin",
            config.base()
        ),
        method: "POST".into(),
        headers: BTreeMap::from([
            ("authorization".into(), "Bearer local-placeholder".into()),
            (
                "x-goog-user-project".into(),
                "unrelated-billing-project".into(),
            ),
        ]),
        metadata: BTreeMap::new(),
        body: Some(STANDARD.encode(br#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#)),
        body_file: None,
    }
}

#[tokio::test]
async fn selected_paygo_headers_are_private_to_the_account_and_do_not_replace_authorization() {
    let dir = support::Directory::new();
    let path = dir.write(&support::person());
    let mut config = support::config(path);
    config
        .headers
        .insert("X-Vertex-AI-LLM-Shared-Request-Type".into(), "flex".into());
    let client = support::Client::new(vec![
        json!({"access_token":"private-access","expires_in":3600}),
        json!({"candidates":[]}),
    ]);
    Vertex::default()
        .fetch_with(request(&config), &client)
        .await
        .unwrap();
    let requests = client.requests.lock().unwrap();
    assert!(!requests[0]
        .headers
        .contains_key("X-Vertex-AI-LLM-Shared-Request-Type"));
    assert_eq!(
        requests[1].headers["X-Vertex-AI-LLM-Shared-Request-Type"],
        "flex"
    );
    assert_eq!(
        requests[1].headers["authorization"],
        "Bearer private-access"
    );
    assert_eq!(requests[1].headers["content-type"], "application/json");
    assert!(!requests[1].headers.contains_key("x-goog-user-project"));
}

#[tokio::test]
async fn explicit_connection_checks_credentials_but_stores_only_the_source_binding() {
    let dir = support::Directory::new();
    let path = dir.write(&support::person());
    let before = std::fs::read(&path).unwrap();
    let client = support::Client::new(vec![
        json!({"access_token":"private-access","expires_in":3600}),
    ]);
    let plugin = Vertex::default();
    let authorized = plugin
        .authorize_with(
            json!({"method":"adc","inputs":{"project":"project-one","sourcePath":path}}),
            &client,
        )
        .await
        .unwrap();
    assert_eq!(authorized["ok"], true);
    assert_eq!(authorized["credentials"]["sourcePath"], json!(path));
    let text = authorized.to_string();
    for secret in ["private-access", "fixture-refresh", "fixture-secret"] {
        assert!(!text.contains(secret));
    }
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[tokio::test]
async fn same_source_mints_once_and_a_replaced_account_cannot_reuse_the_cached_token() {
    let dir = support::Directory::new();
    let path = dir.write(&support::person());
    let config = support::config(path);
    let client = Arc::new(support::Client::new(vec![
        json!({"access_token":"private-access","expires_in":3600}),
    ]));
    let plugin = Arc::new(Vertex::default());
    let work = (0..16).map(|_| {
        let plugin = plugin.clone();
        let client = client.clone();
        let config = config.clone();
        async move {
            plugin
                .token(&config, client.as_ref(), false)
                .await
                .unwrap()
                .access
        }
    });
    for token in futures::future::join_all(work).await {
        assert_eq!(token, "private-access");
    }
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    dir.write(&json!({"type":"authorized_user","client_id":"fixture-client","refresh_token":"someone-else"}));
    assert!(plugin.token(&config, client.as_ref(), false).await.is_err());
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn live_models_preserve_pagination_project_location_and_remote_identity() {
    let dir = support::Directory::new();
    let path = dir.write(&support::person());
    let mut config = support::config(path);
    config.location = "eu".into();
    let client = support::Client::new(vec![
        json!({"access_token":"access","expires_in":3600}),
        json!({"publisherModels":[{"name":"publishers/google/models/gemini-live","displayName":"Live model","inputTokenLimit":200000}],"nextPageToken":"page-two"}),
        json!({"publisherModels":[{"name":"publishers/google/models/gemini-live"},{"name":"publishers/google/models/gemini-new"},{"name":"publishers/google/models/not-a-gemini"}]}),
    ]);
    let response = Vertex::default()
        .models_with(context(&config), &client)
        .await
        .unwrap();
    assert_eq!(response["source"], "remote");
    assert_eq!(response["models"].as_array().unwrap().len(), 2);
    assert_eq!(response["models"][0]["contextWindow"], 200000);
    assert_eq!(response["models"][0]["baseUrl"], config.base());
    let requests = client.requests.lock().unwrap();
    assert!(requests[1]
        .url
        .starts_with("https://aiplatform.eu.rep.googleapis.com/v1/publishers/google/models?"));
    assert!(requests[2].url.contains("pageToken=page-two"));
    assert!(!requests[1].headers.contains_key("x-goog-user-project"));
}

#[tokio::test]
async fn native_gemini_payload_and_stream_are_preserved_without_local_or_quota_credentials() {
    let dir = support::Directory::new();
    let path = dir.write(&support::person());
    let config = support::config(path);
    let mut request = request(&config);
    let body=br#"{"contents":[{"role":"user","parts":[{"functionResponse":{"name":"tool","response":{"ok":true}}},{"inlineData":{"mimeType":"application/pdf","data":"cGRm"}}]}]}"#;
    request.body = Some(STANDARD.encode(body));
    let client = support::Client::new(vec![
        json!({"access_token":"vertex-access","expires_in":3600}),
        json!({"candidates":[]}),
    ]);
    let response = Vertex::default()
        .fetch_with(request, &client)
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[1].body.as_deref(), Some(body.as_slice()));
    assert_eq!(requests[1].headers["authorization"], "Bearer vertex-access");
    assert!(!requests[1].headers.contains_key("x-goog-user-project"));
    assert!(!requests[1].url.contains("key="));
}

#[tokio::test]
async fn foreign_projects_hosts_and_operations_are_rejected_before_minting() {
    let dir = support::Directory::new();
    let path = dir.write(&support::person());
    let config = support::config(path);
    let client = support::Client::new(vec![]);
    for url in [
        "https://evil.example/models/gemini-live:generateContent".into(),
        request(&config)
            .url
            .replace("project-one", "another-project"),
        request(&config)
            .url
            .replace(":streamGenerateContent", ":delete"),
        format!(
            "{}/models/gemini-live:generateContent?api_key=private",
            config.base()
        ),
    ] {
        let mut request = request(&config);
        request.url = url;
        assert!(Vertex::default()
            .fetch_with(request, &client)
            .await
            .is_err());
    }
    assert!(client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn initialization_does_not_discover_or_import_an_adc_account() {
    let plugin = Vertex::default();
    let dir = support::Directory::new();
    let manifest: Value = serde_json::from_str(include_str!("../nora-provider.json")).unwrap();
    plugin
        .initialize(InitializeParams {
            api_version: 1,
            plugin_id: "google-vertex".into(),
            plugin_directory: dir.0.clone(),
            data_directory: dir.0.clone(),
            manifest: manifest.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        plugin.auth_methods(CallContext::default()).await.unwrap(),
        manifest["authMethods"]
    );
    assert_eq!(
        plugin.accounts(CallContext::default()).await.unwrap(),
        json!([])
    );
    assert!(std::fs::read_dir(&dir.0).unwrap().next().is_none());
}
