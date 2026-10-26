use super::*;
use base64::Engine;
use std::sync::atomic::{AtomicBool, Ordering};
#[path = "fixture.rs"]
mod fixture;
#[derive(Default)]
struct Saver {
    fail: AtomicBool,
    values: std::sync::Mutex<Vec<Value>>,
}
#[async_trait]
impl Persistence for Saver {
    async fn save(&self, value: Value) -> PluginResult<()> {
        self.values.lock().unwrap().push(value);
        if self.fail.swap(false, Ordering::SeqCst) {
            Err(PluginError::new(
                "host_save_failed",
                "The host could not save credentials.",
            ))
        } else {
            Ok(())
        }
    }
}
async fn provider(replies: Vec<fixture::Reply>) -> (Kiro, Arc<fixture::Client>, Arc<Saver>) {
    let client = Arc::new(fixture::Client::new(replies));
    let saver = Arc::new(Saver::default());
    let mut provider = Kiro::default();
    provider.client = Some(client.clone());
    provider.persistence = saver.clone();
    provider
        .initialize(InitializeParams {
            subscription_accounts_enabled: true,
            installed_manifest: None,
            runtime_config: serde_json::Value::Null,
            api_version: API_VERSION,
            plugin_id: "kiro".into(),
            plugin_directory: Default::default(),
            data_directory: Default::default(),
            manifest: serde_json::from_str(include_str!("../nora-provider.json")).unwrap(),
        })
        .await
        .unwrap();
    (provider, client, saver)
}
fn context(credentials: Value) -> CallContext {
    CallContext {
        subscription_accounts_enabled: true,
        account: "work".into(),
        credentials: Some(credentials),
        network_proxy: None,
        ..Default::default()
    }
}
fn fetch(credentials: Value) -> FetchRequest {
    FetchRequest{ caller_user_agent: None, constraints: Default::default(), subscription_accounts_enabled: true,account:"work".into(),credentials:Some(credentials),network_proxy:None,url:format!("{}/v1/messages",session::BASE),method:"POST".into(),headers:BTreeMap::from([("authorization".into(),"caller-secret".into())]),metadata:BTreeMap::from([("reasoningEffort".into(),"high".into()),("sessionID".into(),"private-session".into())]),body:Some(base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&json!({"model":"auto","stream":true,"messages":[{"role":"user","content":"hello"}]})).unwrap())),body_file:None}
}
#[tokio::test]
async fn startup_and_disconnected_catalog_do_not_read_or_import_accounts() {
    let (provider, client, _) = provider(vec![]).await;
    assert!(provider
        .accounts(CallContext::default())
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        provider.models(CallContext::default()).await.unwrap()["source"],
        "preset"
    );
    assert!(client.requests.lock().unwrap().is_empty());
    let methods = provider.auth_methods(CallContext::default()).await.unwrap();
    assert_eq!(methods.as_array().unwrap().len(), 5);
}
#[tokio::test]
async fn models_usage_and_chat_use_the_account_region_and_do_not_forward_private_metadata() {
    let(provider,client,_)=provider(vec![fixture::Reply::json(json!({"models":[{"modelId":"auto","tokenLimits":{"maxInputTokens":200000,"maxOutputTokens":32000},"supportedInputTypes":["IMAGE"]}]})),fixture::Reply::json(fixture::usage()),fixture::Reply::raw(200,fixture::event("assistantResponseEvent",json!({"content":"hello"})))]).await;
    let credentials = fixture::saved();
    let models = provider.models(context(credentials.clone())).await.unwrap();
    assert_eq!(models["models"][0]["contextWindow"], 200000);
    let usage = provider.usage(context(credentials.clone())).await.unwrap();
    assert_eq!(usage["account"]["id"], "work");
    let response = provider.fetch(fetch(credentials)).await.unwrap();
    assert_eq!(response.status, 200);
    assert!(fixture::body(response).await.contains("hello"));
    let requests = client.requests.lock().unwrap();
    assert!(requests
        .iter()
        .all(|request| request.url.contains(".eu-central-1.kiro.dev/")));
    let last = requests.last().unwrap();
    assert_eq!(
        last.url,
        "https://runtime.eu-central-1.kiro.dev/generateAssistantResponse"
    );
    assert_eq!(last.headers["authorization"], "Bearer private-access");
    assert_eq!(last.headers["x-amzn-codewhisperer-optout"], "true");
    assert_eq!(last.headers["amz-sdk-request"], "attempt=1; max=1");
    let body: Value = serde_json::from_slice(last.body.as_ref().unwrap()).unwrap();
    assert!(body
        .pointer("/conversationState/currentMessage/userInputMessage/content")
        .unwrap()
        .as_str()
        .unwrap()
        .contains("<max_thinking_length>30000"));
    assert!(!body.to_string().contains("private-session"));
    assert!(!last
        .headers
        .values()
        .any(|value| value.contains("caller-secret")));
}
#[tokio::test]
async fn rotations_wait_for_host_ack_and_retry_the_pending_save_without_reusing_refresh() {
    let (provider, client, saver) = provider(vec![
        fixture::Reply::json(
            json!({"accessToken":"next-access","refreshToken":"next-refresh","expiresIn":3600}),
        ),
        fixture::Reply::json(fixture::usage()),
    ])
    .await;
    let mut credentials = fixture::saved();
    credentials["expires"] = json!(session::now() - 1);
    saver.fail.store(true, Ordering::SeqCst);
    assert_eq!(
        provider
            .usage(context(credentials.clone()))
            .await
            .unwrap_err()
            .code,
        "host_save_failed"
    );
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    let usage = provider.usage(context(credentials)).await.unwrap();
    assert_eq!(usage["available"], true);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].url.ends_with("/refreshToken"));
    assert_eq!(requests[1].headers["authorization"], "Bearer next-access");
    let saved = saver.values.lock().unwrap();
    assert_eq!(saved.len(), 2);
    assert_eq!(saved[0]["refresh"], "next-refresh");
    assert_eq!(saved[1]["access"], "next-access");
}
#[tokio::test]
async fn a_403_refreshes_an_owned_account_once_after_ack_before_retrying_chat() {
    let (provider, client, saver) = provider(vec![
        fixture::Reply::raw(403, b"private-access denied".to_vec()),
        fixture::Reply::json(
            json!({"accessToken":"next-access","refreshToken":"next-refresh","expiresIn":3600}),
        ),
        fixture::Reply::raw(
            200,
            fixture::event("assistantResponseEvent", json!({"content":"answer"})),
        ),
    ])
    .await;
    let response = provider.fetch(fetch(fixture::saved())).await.unwrap();
    assert_eq!(response.status, 200);
    assert!(fixture::body(response).await.contains("answer"));
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[0].headers["authorization"],
        "Bearer private-access"
    );
    assert!(requests[1]
        .url
        .contains("prod.eu-west-1.auth.desktop.kiro.dev/refreshToken"));
    assert_eq!(requests[2].headers["authorization"], "Bearer next-access");
    assert_eq!(saver.values.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn api_key_refusals_do_not_attempt_refresh_or_store_credentials() {
    let (provider, client, saver) = provider(vec![fixture::Reply::raw(
        403,
        b"private-key denied".to_vec(),
    )])
    .await;
    let mut credentials = fixture::saved();
    credentials["method"] = json!("apikey");
    let response = provider.fetch(fetch(credentials)).await.unwrap();
    assert_eq!(response.status, 403);
    assert!(!fixture::body(response).await.contains("private-key"));
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        requests[0].url,
        "https://runtime.eu-central-1.kiro.dev/generateAssistantResponse"
    );
    assert_eq!(requests[0].headers["tokentype"], "API_KEY");
    assert!(
        serde_json::from_slice::<Value>(requests[0].body.as_ref().unwrap())
            .unwrap()
            .get("profileArn")
            .is_none()
    );
    assert!(saver.values.lock().unwrap().is_empty());
}
#[tokio::test]
async fn selected_source_is_read_only_bound_to_identity_and_never_copies_tokens() {
    let directory = std::env::temp_dir().join(format!(
        "nora-kiro-provider-{}",
        nora_ai::oauth_pkce::random_hex(12)
    ));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("kiro-auth-token.json");
    let original=json!({"accessToken":"selected-token","refreshToken":"selected-refresh","expiresAt":session::now()+3600000,"authMethod":"social","profileArn":fixture::PROFILE}).to_string();
    std::fs::write(&path, &original).unwrap();
    let mut changed = fixture::usage();
    changed["userInfo"]["email"] = json!("different@example.test");
    let (provider, client, saver) = provider(vec![
        fixture::Reply::json(fixture::usage()),
        fixture::Reply::json(changed),
    ])
    .await;
    let result = provider
        .authorize(
            json!({"authMethodID":"ideSource","inputs":{"sourcePath":path.to_str().unwrap()}}),
        )
        .await
        .unwrap();
    let credentials = result["credentials"].clone();
    assert!(!credentials.to_string().contains("selected-token"));
    assert!(!credentials.to_string().contains("selected-refresh"));
    assert_eq!(credentials["source"]["kind"], "ide");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    let mut next: Value = serde_json::from_str(&original).unwrap();
    next["accessToken"] = json!("another-token");
    std::fs::write(&path, next.to_string()).unwrap();
    let error = provider.usage(context(credentials)).await.unwrap_err();
    assert_eq!(error.code, "authentication_required");
    assert_eq!(client.requests.lock().unwrap().len(), 2);
    assert!(saver.values.lock().unwrap().is_empty());
    std::fs::remove_dir_all(directory).unwrap();
}
#[tokio::test]
async fn source_authorization_accepts_verified_user_id_without_email_and_keeps_tokens_in_the_source(
) {
    for method in ["cliSource", "ideSource"] {
        let directory = std::env::temp_dir().join(format!(
            "nora-kiro-user-id-source-{}",
            nora_ai::oauth_pkce::random_hex(12)
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join(if method == "cliSource" {
            "data.sqlite3"
        } else {
            "kiro-auth-token.json"
        });
        if method == "cliSource" {
            let db = rusqlite::Connection::open(&path).unwrap();
            db.execute_batch("CREATE TABLE auth_kv (key TEXT PRIMARY KEY,value TEXT NOT NULL)")
                .unwrap();
            db.execute("INSERT INTO auth_kv VALUES (?1,?2)",["kirocli:social:token",&json!({"access_token":"source-only-private-access","refresh_token":"source-only-private-refresh","expires_at":session::now()+3_600_000,"profile_arn":fixture::PROFILE}).to_string()]).unwrap();
        } else {
            std::fs::write(&path,json!({"accessToken":"source-only-private-access","refreshToken":"source-only-private-refresh","expiresAt":session::now()+3_600_000,"authMethod":"social","profileArn":fixture::PROFILE}).to_string()).unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let (provider, client, saver) = provider(vec![fixture::Reply::json(
            json!({"userInfo":{"userId":"verified-source-user"}}),
        )])
        .await;
        let result = provider
            .authorize(
                json!({"authMethodID":method,"inputs":{"sourcePath":path.to_str().unwrap()}}),
            )
            .await
            .unwrap();
        assert_eq!(result["ok"], true);
        let credentials = &result["credentials"];
        assert_eq!(
            credentials["accountIdentity"]["subject"],
            "user:verified-source-user"
        );
        assert_eq!(credentials["profileArn"], fixture::PROFILE);
        assert_eq!(credentials["email"], "");
        assert_eq!(
            credentials["source"]["kind"],
            if method == "cliSource" { "cli" } else { "ide" }
        );
        assert_eq!(credentials["source"]["path"], path.to_str().unwrap());
        assert_eq!(credentials["extras"]["noraAccountSource"]["kind"], method);
        for field in ["access", "refresh", "key", "clientSecret", "tokenEndpoint"] {
            assert!(credentials.get(field).is_none());
        }
        assert!(!credentials.to_string().contains("source-only-private"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(client.requests.lock().unwrap().len(), 1);
        assert!(saver.values.lock().unwrap().is_empty());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
#[tokio::test]
async fn a_magpie_source_reference_survives_authorize_account_enrichment_and_usage_without_copying_tokens(
) {
    let directory = std::env::temp_dir().join(format!(
        "nora-kiro-magpie-consumer-{}",
        nora_ai::oauth_pkce::random_hex(12)
    ));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("plugin-auth.json");
    let original = json!({
        "kiro#selected": {
            "type":"oauth", "access":"private-magpie-access", "refresh":"private-magpie-refresh",
            "expires":session::now()+3_600_000, "method":"social",
            "profileArn":fixture::PROFILE, "region":"eu-central-1",
            "accountId":"person@example.test (Google)", "uid":"google", "loginProvider":"Google"
        }
    })
    .to_string();
    std::fs::write(&path, &original).unwrap();
    let mut usage = fixture::usage();
    usage["userInfo"]["userId"] = json!("verified-magpie-user");
    let (provider, client, saver) = provider(vec![
        fixture::Reply::json(usage.clone()),
        fixture::Reply::json(usage.clone()),
        fixture::Reply::json(usage),
    ])
    .await;
    let authorized = provider
        .authorize(json!({
            "authMethodID":"magpieSource",
            "inputs":{"sourcePath":path,"sourceAccount":"kiro#selected"}
        }))
        .await
        .unwrap();
    let credentials = authorized["credentials"].clone();
    assert_eq!(
        credentials["source"],
        json!({"kind":"magpie","path":path,"account":"kiro#selected"})
    );
    let enriched = provider
        .accounts(context(credentials.clone()))
        .await
        .unwrap();
    let saved = enriched.get("credentials").cloned().unwrap_or(credentials);
    assert_eq!(saved["source"]["kind"], "magpie");
    assert_eq!(saved["source"]["account"], "kiro#selected");
    assert_eq!(saved["source"]["path"], path.to_str().unwrap());
    assert_eq!(
        saved["extras"]["noraAccountSource"]["account"],
        "kiro#selected"
    );
    assert_eq!(
        saved["accountIdentity"]["subject"],
        "user:verified-magpie-user"
    );
    assert!(saved.get("access").is_none());
    assert!(saved.get("refresh").is_none());
    assert!(!saved.to_string().contains("private-"));
    let usage = provider.usage(context(saved)).await.unwrap();
    assert_eq!(usage["account"]["id"], "work");
    assert_eq!(client.requests.lock().unwrap().len(), 3);
    assert!(saver.values.lock().unwrap().is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn expired_sources_without_renewal_credentials_do_not_make_upstream_requests() {
    let path = std::env::temp_dir().join(format!(
        "nora-kiro-expired-{}",
        nora_ai::oauth_pkce::random_hex(12)
    ));
    std::fs::write(
        &path,
        json!({"accessToken":"expired-token","expiresAt":session::now()-1}).to_string(),
    )
    .unwrap();
    let (provider, client, saver) = provider(vec![]).await;
    let result = provider
        .authorize(
            json!({"authMethodID":"ideSource","inputs":{"sourcePath":path.to_str().unwrap()}}),
        )
        .await;
    assert!(result.is_err());
    assert!(client.requests.lock().unwrap().is_empty());
    assert!(saver.values.lock().unwrap().is_empty());
    std::fs::remove_file(path).unwrap();
}
#[tokio::test]
async fn invalid_endpoints_proxy_and_query_fail_before_any_upstream_request() {
    let (provider, client, _) = provider(vec![]).await;
    for url in [
        "https://foreign.invalid/v1/messages",
        "https://user:secret@runtime.us-east-1.kiro.dev/v1/messages",
        "https://runtime.us-east-1.kiro.dev/v1/messages?endpoint=evil",
        "https://runtime.us-east-1.kiro.dev/other",
    ] {
        let mut request = fetch(fixture::saved());
        request.url = url.into();
        assert!(provider.fetch(request).await.is_err());
    }
    let mut request = fetch(fixture::saved());
    request.network_proxy = Some(NetworkProxy::Proxy {
        url: "ftp://user:private-password@proxy.invalid".into(),
    });
    let error = provider.fetch(request).await.err().unwrap();
    assert!(!error.message.contains("private-password"));
    assert!(client.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn api_key_authorization_uses_real_profile_and_account_queries_and_redacts_accounts() {
    let (provider, client, _) = provider(vec![
        fixture::Reply::json(json!({"profile":{"arn":fixture::PROFILE}})),
        fixture::Reply::json(fixture::usage()),
    ])
    .await;
    let result = provider
        .authorize(json!({"authMethodID":"apiKey","inputs":{"apiKey":"ksk_private"}}))
        .await
        .unwrap();
    let accounts = provider
        .accounts(context(result["credentials"].clone()))
        .await
        .unwrap();
    assert_eq!(accounts["accounts"][0]["email"], "person@example.test");
    assert!(!accounts["accounts"].to_string().contains("ksk_private"));
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].headers["tokentype"], "API_KEY");
    assert_eq!(requests[1].headers["tokentype"], "API_KEY");
}

#[tokio::test]
async fn an_api_key_with_no_profile_or_email_can_authorize_list_models_read_usage_and_chat() {
    let (provider, client, saver) = provider(vec![
        fixture::Reply::raw(400, b"No profile in this region".to_vec()),
        fixture::Reply::raw(400, b"No profile in this region".to_vec()),
        fixture::Reply::json(json!({"usageBreakdownList":[]})),
        fixture::Reply::json(json!({"models":[{"modelId":"auto","modelName":"Auto"}]})),
        fixture::Reply::json(fixture::usage()),
        fixture::Reply::raw(
            200,
            fixture::event("assistantResponseEvent", json!({"content":"answer"})),
        ),
    ])
    .await;
    let authorization = provider
        .authorize(json!({"authMethodID":"apiKey","inputs":{"apiKey":"ksk_private_no_profile"}}))
        .await
        .unwrap();
    let credentials = authorization["credentials"].clone();
    assert_eq!(authorization["ok"], true);
    assert_eq!(credentials["type"], "api");
    assert_eq!(credentials["method"], "apikey");
    assert_eq!(
        credentials["accountId"],
        network::key_identity("ksk_private_no_profile")
    );
    assert_eq!(credentials["profileArn"], "");
    assert_eq!(credentials["email"], "");
    assert_eq!(credentials["userId"], "");
    assert!(credentials.get("accountIdentity").is_none());
    let accounts = provider
        .accounts(context(credentials.clone()))
        .await
        .unwrap();
    assert_eq!(
        accounts["accounts"][0]["accountId"],
        credentials["accountId"]
    );
    assert!(!accounts["accounts"].to_string().contains("ksk_private"));
    assert_eq!(
        provider.models(context(credentials.clone())).await.unwrap()["models"][0]["id"],
        "auto"
    );
    assert_eq!(
        provider.usage(context(credentials.clone())).await.unwrap()["account"]["id"],
        "work"
    );
    let response = provider.fetch(fetch(credentials)).await.unwrap();
    assert_eq!(response.status, 200);
    assert!(fixture::body(response).await.contains("answer"));
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 6);
    assert_eq!(
        requests[2].headers["x-amz-target"],
        "AmazonCodeWhispererService.GetUsageLimits"
    );
    assert_eq!(
        requests[3].headers["x-amz-target"],
        "KiroControlPlaneBearerService.ListAvailableModels"
    );
    assert_eq!(
        requests[4].headers["x-amz-target"],
        "AmazonCodeWhispererService.GetUsageLimits"
    );
    assert_eq!(
        requests[5].url,
        "https://runtime.us-east-1.kiro.dev/generateAssistantResponse"
    );
    let payload: Value = serde_json::from_slice(requests[5].body.as_ref().unwrap()).unwrap();
    assert!(payload.get("profileArn").is_none());
    assert!(requests
        .iter()
        .all(|request| request.headers["tokentype"] == "API_KEY"));
    assert!(saver.values.lock().unwrap().is_empty());
}

#[tokio::test]
async fn api_key_metadata_failure_does_not_reject_the_key_or_invent_a_user_identity() {
    let (provider, client, _) = provider(vec![
        fixture::Reply::json(json!({"profile":{"arn":fixture::PROFILE}})),
        fixture::Reply::raw(403, b"Metadata is not available for this key".to_vec()),
    ])
    .await;
    let authorization = provider
        .authorize(json!({"authMethodID":"apiKey","inputs":{"apiKey":"ksk_private_metadata"}}))
        .await
        .unwrap();
    let credentials = authorization["credentials"].clone();
    assert_eq!(authorization["ok"], true);
    assert_eq!(credentials["type"], "api");
    assert_eq!(credentials["method"], "apikey");
    assert_eq!(credentials["profileArn"], fixture::PROFILE);
    assert_eq!(
        credentials["accountId"],
        network::key_identity("ksk_private_metadata")
    );
    assert_eq!(credentials["userId"], "");
    assert_eq!(credentials["email"], "");
    assert!(credentials.get("accountIdentity").is_none());
    let accounts = provider.accounts(context(credentials)).await.unwrap();
    assert!(accounts["accounts"][0].get("email").is_none());
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_standard_saved_api_key_without_an_explicit_method_keeps_its_auth_type_and_can_chat() {
    let (provider, client, saver) = provider(vec![
        fixture::Reply::raw(400, b"No profile".to_vec()),
        fixture::Reply::raw(400, b"No profile".to_vec()),
        fixture::Reply::raw(
            200,
            fixture::event(
                "assistantResponseEvent",
                json!({"content":"standard key works"}),
            ),
        ),
    ])
    .await;
    let credentials = json!({"type":"api","key":"ksk_private_standard"});
    let account = provider
        .accounts(context(credentials.clone()))
        .await
        .unwrap();
    assert_eq!(account["credentials"]["method"], "apikey");
    assert_eq!(account["credentials"]["type"], "api");
    assert_eq!(
        account["credentials"]["accountId"],
        network::key_identity("ksk_private_standard")
    );
    assert!(account["credentials"].get("accountIdentity").is_none());
    let response = provider.fetch(fetch(credentials)).await.unwrap();
    assert!(fixture::body(response).await.contains("standard key works"));
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests
        .iter()
        .all(|request| request.headers["tokentype"] == "API_KEY"));
    assert!(
        serde_json::from_slice::<Value>(requests[2].body.as_ref().unwrap())
            .unwrap()
            .get("profileArn")
            .is_none()
    );
    assert!(saver.values.lock().unwrap().is_empty());
}
#[tokio::test]
async fn explicit_refresh_uses_the_same_acknowledged_rotation_path() {
    let (provider, client, saver) = provider(vec![fixture::Reply::json(
        json!({"accessToken":"next","refreshToken":"rotated","expiresIn":3600}),
    )])
    .await;
    let result = provider
        .refresh(json!({"credentials":fixture::saved(),"force":true}))
        .await
        .unwrap();
    assert_eq!(result["changed"], false);
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    assert_eq!(saver.values.lock().unwrap()[0]["refresh"], "rotated");
}

#[tokio::test]
async fn an_owned_rotation_survives_identity_service_failure_without_replaying_its_old_refresh_token(
) {
    let (provider, client, saver) = provider(vec![
        fixture::Reply::json(json!({"accessToken":"next-owned-access","refreshToken":"next-owned-refresh","expiresIn":3600})),
        fixture::Reply::raw(503, b"private-identity-service-failure".to_vec()),
        fixture::Reply::json(fixture::usage()),
    ]).await;
    let mut credentials = fixture::saved();
    credentials["expires"] = json!(session::now() - 1);
    credentials["accountIdentity"] = Credential::from_saved(&credentials)
        .unwrap()
        .account_identity()
        .unwrap();
    credentials["accountId"] = json!("");
    let error = provider
        .live(Some(&credentials), client.as_ref(), false)
        .await
        .err()
        .unwrap();
    assert!(!error.message.contains("private-"));
    assert_eq!(client.requests.lock().unwrap().len(), 2);
    assert!(saver.values.lock().unwrap().is_empty());
    let credential = provider
        .live(Some(&credentials), client.as_ref(), false)
        .await
        .unwrap();
    assert_eq!(credential.access, "next-owned-access");
    assert_eq!(credential.refresh, "next-owned-refresh");
    let repeated = provider
        .live(Some(&credentials), client.as_ref(), false)
        .await
        .unwrap();
    assert_eq!(repeated.refresh, "next-owned-refresh");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.url.ends_with("/refreshToken"))
            .count(),
        1
    );
    assert_eq!(
        requests[2].headers["authorization"],
        "Bearer next-owned-access"
    );
    let saved = saver.values.lock().unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0]["refresh"], "next-owned-refresh");
    assert_eq!(saved[0]["accountIdentity"], credentials["accountIdentity"]);
}

#[tokio::test]
async fn an_owned_refresh_cannot_change_profile_for_the_same_email_before_storage_acknowledgment() {
    let other_profile = "arn:aws:codewhisperer:eu-central-1:123456789012:profile/other";
    let (provider, client, saver) = provider(vec![
        fixture::Reply::json(json!({"accessToken":"other-profile-access","refreshToken":"other-profile-refresh","expiresIn":3600,"profileArn":other_profile})),
        fixture::Reply::json(fixture::usage()),
    ]).await;
    let mut credentials = fixture::saved();
    credentials["expires"] = json!(session::now() - 1);
    let credential = Credential::from_saved(&credentials).unwrap();
    credentials["accountIdentity"] = credential.account_identity().unwrap();
    assert!(provider
        .live(Some(&credentials), client.as_ref(), false)
        .await
        .is_err());
    assert_eq!(client.requests.lock().unwrap().len(), 2);
    assert!(saver.values.lock().unwrap().is_empty());
    assert!(provider.rotations.lock().await.after.is_empty());
    assert_eq!(provider.rotations.lock().await.pending.len(), 1);
    assert!(provider
        .live(Some(&credentials), client.as_ref(), false)
        .await
        .is_err());
    assert_eq!(client.requests.lock().unwrap().len(), 2);
    assert!(saver.values.lock().unwrap().is_empty());
}

#[tokio::test]
async fn owned_rotation_caches_do_not_cross_profiles_that_happen_to_share_an_access_token() {
    let (provider, client, saver) = provider(vec![fixture::Reply::json(
        json!({"accessToken":"renewed-first","refreshToken":"refresh-first","expiresIn":3600}),
    )])
    .await;
    let first = fixture::saved();
    provider
        .live(Some(&first), client.as_ref(), true)
        .await
        .unwrap();
    let mut second = first.clone();
    second["profileArn"] = json!("arn:aws:codewhisperer:eu-central-1:123456789012:profile/second");
    let current = provider
        .live(Some(&second), client.as_ref(), false)
        .await
        .unwrap();
    assert_eq!(current.access, "private-access");
    assert_eq!(current.profile, second["profileArn"].as_str().unwrap());
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    assert_eq!(saver.values.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn legacy_login_provider_identity_is_saved_on_first_use_without_waiting_for_expiry() {
    let (provider, client, saver) = provider(vec![]).await;
    let mut old = fixture::saved();
    old["accountIdentity"] = Credential::from_saved(&old)
        .unwrap()
        .account_identity()
        .unwrap();
    old["loginProvider"] = json!("Google");
    let first = provider
        .live(Some(&old), client.as_ref(), false)
        .await
        .unwrap();
    assert!(client.requests.lock().unwrap().is_empty());
    let saved = saver.values.lock().unwrap()[0].clone();
    assert_eq!(
        saved["accountIdentity"]["discriminator"],
        "login-provider:google"
    );
    assert_eq!(saved["access"], old["access"]);
    assert_eq!(saved["refresh"], old["refresh"]);
    assert!(first.matches_saved_identity(&old));
    let again = provider
        .live(Some(&saved), client.as_ref(), false)
        .await
        .unwrap();
    assert_eq!(again.account_identity(), first.account_identity());
    assert_eq!(saver.values.lock().unwrap().len(), 1);
}
