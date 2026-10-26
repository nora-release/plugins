use super::*;
use crate::test_support::{Client, Directory};
use base64::Engine;

async fn plugin(directory: &Directory, client: Arc<Client>) -> GcpAi {
    let plugin = GcpAi {
        cli: directory.cli(BTreeMap::new()),
        client: Some(client),
        ..GcpAi::default()
    };
    plugin
        .initialize(InitializeParams {
            api_version: 1,
            subscription_accounts_enabled: true,
            plugin_id: "gcp-ai".into(),
            plugin_directory: directory.0.clone(),
            data_directory: directory.0.join("data"),
            manifest: serde_json::from_str(include_str!("../nora-provider.json")).unwrap(),
            installed_manifest: None,
            runtime_config: Value::Null,
        })
        .await
        .unwrap();
    plugin
}
fn request(settings: &Settings, body: &[u8]) -> FetchRequest {
    serde_json::from_value(json!({"url":format!("{}/models/gemini-4-argon:streamGenerateContent?alt=sse",settings.endpoint(settings.project.as_deref().unwrap()).unwrap()),"method":"POST","account":"saved","credentials":credentials::record(settings,None),"headers":{"Authorization":"caller-secret","x-goog-api-key":"caller-google-key","X-Trace":"kept","cookie":"caller-cookie"},"metadata":{"sessionID":"private-conversation"},"body":base64::engine::general_purpose::STANDARD.encode(body)})).unwrap()
}

#[tokio::test]
async fn native_gemini_routes_accept_only_this_accounts_publisher_endpoint() {
    let directory = Directory::new();
    let settings = directory.settings();
    let payload = br#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
    let valid = request(&settings, payload);
    let built = inference(valid, &settings, "fixture-token").unwrap();
    assert_eq!(built.body.as_deref(), Some(payload.as_slice()));
    assert_eq!(built.headers["authorization"], "Bearer fixture-token");
    assert_eq!(built.headers["x-goog-user-project"], "fixture-project");
    assert_eq!(built.headers["X-Trace"], "kept");
    for url in ["https://evil.invalid/models/model:generateContent","https://aiplatform.googleapis.com/v1beta1/projects/other-project/locations/global/publishers/google/models/model:generateContent","https://aiplatform.googleapis.com/v1beta1/projects/fixture-project/locations/global/publishers/google/other/route","https://aiplatform.googleapis.com/v1beta1/projects/fixture-project/locations/global/publishers/google/models/model:delete","https://aiplatform.googleapis.com/v1beta1/projects/fixture-project/locations/global/publishers/google/models/model:generateContent#fragment"]{
        let mut request=request(&settings,payload);request.url=url.into();assert!(inference(request,&settings,"fixture-token").is_err());
    }
    for path in [
        "/models",
        "/models/gemini-model",
        "/models/gemini-model:generateContent",
        "/models/gemini-model:streamGenerateContent",
        "/models/gemini-model:countTokens",
    ] {
        assert!(gemini_route(path));
    }
    assert!(built.headers.keys().all(|name| !matches!(
        name.to_ascii_lowercase().as_str(),
        "cookie" | "x-goog-api-key" | "x-api-key"
    )));
}

#[cfg(unix)]
#[tokio::test]
async fn actual_fetch_keeps_thought_signatures_tool_results_and_streaming_bytes_and_probes_once() {
    let directory = Directory::new();
    let settings = directory.settings();
    let client = Arc::new(Client::new(|request, _| {
        crate::test_support::user_info(request).unwrap_or((200, json!({"candidates":[]})))
    }));
    let plugin = plugin(&directory, client.clone()).await;
    let payload=br#"{"contents":[{"role":"model","parts":[{"functionCall":{"name":"tool","args":{"path":"x"}},"thoughtSignature":"opaque-signature"}]},{"role":"user","parts":[{"functionResponse":{"name":"tool","response":{"result":"ok"}}}]}]}"#;
    for _ in 0..2 {
        let response = plugin.fetch(request(&settings, payload)).await.unwrap();
        assert_eq!(response.status, 200);
    }
    let calls = client.calls.lock().unwrap();
    assert_eq!(calls.len(), 5);
    assert_eq!(
        calls
            .iter()
            .filter(|request| request.url.ends_with("/locations/global"))
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|request| request.url.contains("countTokens"))
            .count(),
        1
    );
    let generations = calls
        .iter()
        .filter(|request| request.url.contains("streamGenerateContent"))
        .collect::<Vec<_>>();
    assert_eq!(generations.len(), 2);
    assert!(generations
        .iter()
        .all(|request| request.body.as_deref() == Some(payload.as_slice())));
    assert!(generations
        .iter()
        .all(|request| request.headers["authorization"] == "Bearer fixture-adc-token"));
    assert!(calls.iter().all(|request| !request
        .headers
        .values()
        .any(|value| value.contains("caller-secret") || value.contains("caller-cookie"))));
}
#[cfg(unix)]
#[tokio::test]
async fn rejected_location_checks_are_not_cached_as_success_and_bad_targets_never_print_a_token() {
    let directory = Directory::new();
    let settings = directory.settings();
    let client = Arc::new(Client::new(|request, _| {
        crate::test_support::user_info(request).unwrap_or((401, json!({"error":"fixture-token"})))
    }));
    let plugin = plugin(&directory, client.clone()).await;
    let mut bad = request(&settings, b"{}");
    bad.url = "https://foreign.invalid/endpoint".into();
    assert!(plugin.fetch(bad).await.is_err());
    assert!(directory.commands().is_empty());
    for _ in 0..2 {
        assert_eq!(
            plugin
                .fetch(request(&settings, b"{}"))
                .await
                .err()
                .unwrap()
                .code,
            "authentication_required"
        );
    }
    assert_eq!(client.calls.lock().unwrap().len(), 3);
    assert!(client
        .calls
        .lock()
        .unwrap()
        .iter()
        .all(|request| !request.url.contains("generateContent")));
}

#[tokio::test]
async fn location_throttling_and_service_failures_remain_retryable_without_endorsing_the_account() {
    let directory = Directory::new();
    let settings = directory.settings();
    for status in [429, 500, 502, 503, 504] {
        let client = Arc::new(Client::new(move |_, _| {
            (status, json!({"secret":"hidden"}))
        }));
        let plugin = GcpAi::default();
        for _ in 0..2 {
            let error = plugin
                .endorse(&settings, "private-token", client.as_ref())
                .await
                .unwrap_err();
            assert_eq!(error.code, "upstream_transport_error");
            assert!(error.to_string().contains("Credentials were kept"));
            assert!(!error.to_string().contains("secret"));
            assert!(plugin.endorsed.lock().await.is_empty());
        }
        assert_eq!(client.calls.lock().unwrap().len(), 2);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn replacing_selected_adc_refuses_generation_without_erasing_the_new_credentials() {
    let directory = Directory::new();
    let settings = directory.settings();
    let file = credentials::adc_file(&settings);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        br#"{"owner":"person@example.invalid","nonce":"first"}"#,
    )
    .unwrap();
    let client = Arc::new(Client::new(|request, _| {
        crate::test_support::user_info(request).unwrap_or((200, json!({"candidates":[]})))
    }));
    let plugin = plugin(&directory, client.clone()).await;
    plugin.fetch(request(&settings, b"{}")).await.unwrap();
    let replaced = br#"{"owner":"different@example.invalid","nonce":"other-person"}"#;
    std::fs::write(&file, replaced).unwrap();
    let error = plugin.fetch(request(&settings, b"{}")).await.err().unwrap();
    assert_eq!(error.code, "authentication_required");
    assert_eq!(std::fs::read(&file).unwrap(), replaced);
    assert_eq!(directory.commands().lines().count(), 2);
    let calls = client.calls.lock().unwrap();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.url.contains("streamGenerateContent"))
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.url.contains("openidconnect.googleapis.com"))
            .count(),
        2
    );
    assert!(!calls
        .iter()
        .filter(|call| !call.url.contains("openidconnect.googleapis.com"))
        .any(|call| call
            .headers
            .values()
            .any(|value| value.contains("different@example.invalid"))));
}
#[cfg(unix)]
#[tokio::test]
async fn account_metadata_uses_the_saved_email_and_model_failure_does_not_replace_the_seed_list() {
    let directory = Directory::new();
    let settings = directory.settings();
    let client = Arc::new(Client::new(|_, _| (503, json!({}))));
    let plugin = plugin(&directory, client).await;
    let credentials = credentials::record(&settings, None);
    let context = CallContext {
        account: "saved-id".into(),
        credentials: Some(credentials.clone()),
        ..CallContext::default()
    };
    let accounts = plugin.accounts(context.clone()).await.unwrap();
    assert_eq!(accounts[0]["id"], "saved-id");
    assert_eq!(accounts[0]["email"], "person@example.invalid");
    assert!(directory.commands().is_empty());
    let seeds = plugin.models(CallContext::default()).await.unwrap();
    assert_eq!(seeds["models"].as_array().unwrap().len(), 12);
    assert!(seeds["models"]
        .as_array()
        .unwrap()
        .iter()
        .all(|model| models::is_chat(model["id"].as_str().unwrap())));
    assert!(plugin.models(context).await.is_err());
    let error = plugin.usage(CallContext::default()).await.unwrap();
    assert_eq!(error["available"], false);
    assert_eq!(error["signIn"], "kept");
}
#[test]
fn credits_usage_preserves_balance_reset_amount_and_identity_without_turning_failure_into_zero() {
    let directory = Directory::new();
    let settings = directory.settings();
    let value = usage(
        json!({"remaining":200.25,"original":300.,"used":99.75,"usedPercent":33.25,"cached":true,"fetchedAt":"2026-10-26T00:00:00Z","runsOutAt":"2026-12-31T00:00:00Z","runsOut":50.25}),
        &settings,
    );
    assert_eq!(value["balance"], "$200.25 USD");
    assert_eq!(value["items"][0]["usedPercent"], 33.25);
    assert_eq!(value["items"][0]["countsForRouting"], true);
    assert_eq!(value["items"][0]["runsOut"], 50.25);
    assert_eq!(value["items"][0]["runsOutAt"], "2026-12-31T00:00:00Z");
    assert_eq!(value["account"]["email"], "person@example.invalid");
    assert!(value["items"][0]["description"]
        .as_str()
        .unwrap()
        .contains("(cached)"));
}
#[cfg(unix)]
#[tokio::test]
async fn pending_authorization_returns_immediately_and_missing_cli_finishes_without_a_browser() {
    let directory = Directory::new();
    let mut plugin = plugin(&directory, Arc::new(Client::new(|_, _| (200, json!({}))))).await;
    plugin.cli = Gcloud::new(directory.0.join("missing-gcloud"), BTreeMap::new());
    let started = Instant::now();
    let result=plugin.authorize(json!({"method":"gcloud","inputs":{"project":"fixture-project","profile":directory.0.join("profile")}})).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(result["method"], "auto");
    assert_eq!(result["url"], "");
    assert!(plugin
        .callback(json!({"session":result["session"]}))
        .await
        .is_err());
    assert!(!directory.0.join("profile").exists());
    assert!(std::fs::read_dir(directory.0.join("data/adc"))
        .unwrap()
        .next()
        .is_none());
}
#[cfg(unix)]
#[tokio::test]
async fn aborting_the_callback_stops_the_adc_writer_before_cleaning_up_its_new_directory() {
    use std::os::unix::fs::PermissionsExt;
    let directory = Directory::new();
    let wait = directory.0.join("wait");
    std::fs::write(&wait, b"wait").unwrap();
    let mut plugin = plugin(
        &directory,
        Arc::new(Client::new(|_, _| {
            (200, json!({"email":"person@example.invalid"}))
        })),
    )
    .await;
    plugin.cli = directory.cli(BTreeMap::from([(
        "FAKE_LOGIN_WAIT".into(),
        wait.to_string_lossy().into_owned(),
    )]));
    let executable = directory.0.join("fake-chrome");
    std::fs::write(&executable, include_bytes!("fixtures/chrome-signin.sh")).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    plugin.browser = Arc::new(BrowserService::with_signin_executable(executable));
    let plugin = Arc::new(plugin);
    let result=plugin.authorize(json!({"method":"gcloud","inputs":{"project":"fixture-project","account":"person@example.invalid","profile":directory.0.join("profile")}})).await.unwrap();
    let task = {
        let plugin = plugin.clone();
        tokio::spawn(async move { plugin.callback(json!({"session":result["session"]})).await })
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        while !directory.0.join("profile/signin-args").is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    let _ = task.await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    std::fs::remove_file(wait).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(std::fs::read_dir(directory.0.join("data/adc"))
        .unwrap()
        .next()
        .is_none());
    assert!(plugin.pending.lock().await.is_empty());
}
