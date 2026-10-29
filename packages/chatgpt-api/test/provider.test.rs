use super::*;
use crate::{
    auth::tests::saved,
    test_support::{Client, Directory, Reply},
};
use base64::Engine;
use futures::StreamExt;
async fn configured(client: Arc<dyn HttpClient>) -> (Directory, ChatGPTAPI) {
    let directory = Directory::new();
    let mut provider = ChatGPTAPI::default();
    provider.client = Some(client);
    provider
        .initialize(InitializeParams {
            api_version: 2,
            subscription_accounts_enabled: true,
            plugin_id: "chatgpt-api".into(),
            plugin_directory: "/test/package".into(),
            data_directory: directory.0.clone(),
            manifest: serde_json::from_str(include_str!("../nora-plugin.json")).unwrap(),
            installed_manifest: None,
            runtime_config: Value::Null,
        })
        .await
        .unwrap();
    (directory, provider)
}
fn context() -> CallContext {
    CallContext {
        account: "saved-account".into(),
        credentials: Some(saved(3600_000)),
        ..CallContext::default()
    }
}
fn request(body: Value) -> FetchRequest {
    serde_json::from_value(json!({"account":"saved-account","credentials":saved(3600_000),"url":format!("{}/responses",auth::BASE),"method":"POST","headers":{"authorization":"foreign-private","cookie":"foreign-private","x-api-key":"foreign-private","chatgpt-account-id":"foreign-private","session_id":"foreign-private","conversation_id":"foreign-private","x-codex-turn-state":"foreign-private","x-nora-secret":"foreign-private","x-client-request-id":"request-one"},"body":base64::engine::general_purpose::STANDARD.encode(body.to_string())})).unwrap()
}
#[tokio::test]
async fn initialize_does_not_import_codex_accounts_or_start_oauth_and_usage_is_not_guessed() {
    let client = Client::new(vec![]);
    let (directory, provider) = configured(client.clone()).await;
    assert_eq!(
        provider.accounts(CallContext::default()).await.unwrap(),
        json!([])
    );
    assert_eq!(
        provider.models(CallContext::default()).await.unwrap()["models"],
        json!([])
    );
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 0);
    assert!(client.requests.lock().unwrap().is_empty());
    let accounts = provider.accounts(context()).await.unwrap();
    assert_eq!(accounts[0]["id"], "saved-account");
    assert_eq!(accounts[0]["accountId"], "member");
    assert_eq!(accounts[0]["email"], "me@example.com");
    let usage = provider.usage(context()).await.unwrap();
    assert_eq!(usage["available"], false);
    assert!(usage.get("items").is_none());
    assert!(usage["links"][0]["url"]
        .as_str()
        .unwrap()
        .contains("chatgpt.com/settings/usage"));
}
#[tokio::test]
async fn live_models_use_the_selected_account_bearer_without_refresh_or_save() {
    let client = Client::new(vec![Reply::json(
        200,
        json!({"models":[{"slug":"gpt-6-sol","visibility":"list","input_modalities":["text","image"]}]}),
    )]);
    let (_directory, provider) = configured(client.clone()).await;
    let value = provider.models(context()).await.unwrap();
    assert_eq!(value["models"][0]["id"], "gpt-6-sol");
    assert_eq!(value["source"], "remote");
    assert_eq!(value["defaultAPI"], "openai-responses");
    assert_eq!(
        client.requests.lock().unwrap()[0].headers["authorization"],
        "Bearer access-one"
    );
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    assert!(provider.saved.lock().await.is_empty());
}
#[tokio::test]
async fn subscription_request_uses_prepared_body_session_headers_and_exact_sse() {
    let sse = "event: response.output_text.delta\ndata: {\"delta\":\"hello\"}\n\ndata: [DONE]\n\n";
    let mut reply = Reply::raw(200, sse);
    reply.headers = BTreeMap::from([
        ("content-encoding".into(), "gzip".into()),
        ("content-length".into(), "1000".into()),
        ("content-type".into(), "text/event-stream".into()),
    ]);
    let client = Client::new(vec![reply]);
    let (_directory, provider) = configured(client.clone()).await;
    let response=provider.fetch(request(json!({"model":"gpt-6-sol","instructions":"Do the task","input":"hi","prompt_cache_key":"thread-one","temperature":0.1,"max_output_tokens":1000,"tools":[{"type":"function","name":"shell"}]}))).await.unwrap();
    assert!(!response.headers.contains_key("content-encoding"));
    assert!(!response.headers.contains_key("content-length"));
    assert_eq!(
        response
            .body
            .map(|b| b.unwrap())
            .collect::<Vec<_>>()
            .await
            .concat(),
        sse.as_bytes()
    );
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].headers["authorization"], "Bearer access-one");
    assert_eq!(requests[0].headers["session-id"], "thread-one");
    assert_eq!(requests[0].headers["thread-id"], "thread-one");
    assert_eq!(requests[0].headers["x-client-request-id"], "request-one");
    assert!(!format!("{:?}", requests[0]).contains("foreign-private"));
    let body: Value = serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(body["instructions"], "Do the task");
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert!(body.get("temperature").is_none());
    assert!(body.get("max_output_tokens").is_none());
    assert_eq!(body["tools"][0]["name"], "shell");
}
#[tokio::test]
async fn foreign_urls_and_per_account_proxy_are_rejected_before_any_token_use() {
    let client = Client::new(vec![]);
    let (_directory, provider) = configured(client.clone()).await;
    for url in [
        "https://api.openai.com/v1/chat/completions",
        "https://api.openai.com/v1/responses?other=1",
        "https://evil.test/v1/responses",
        "http://api.openai.com/v1/responses",
    ] {
        let mut request = request(json!({}));
        request.url = url.into();
        request.credentials = Some(saved(-1));
        assert!(provider.fetch(request).await.is_err());
    }
    let mut context = context();
    context.network_proxy = Some(NetworkProxy::Proxy {
        url: "file:///private".into(),
    });
    assert!(provider.models(context).await.is_err());
    assert!(client.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn upstream_errors_are_explained_scrubbed_and_not_retried_by_the_plugin() {
    for status in [400, 401, 429, 503] {
        let client = Client::new(vec![Reply::json(
            status,
            json!({"error":{"code":"subscription_sharing_usage_limit_exceeded","message":"access-one refresh-one"}}),
        )]);
        let (_directory, provider) = configured(client.clone()).await;
        let response = provider
            .fetch(request(json!({"input":"hi"})))
            .await
            .unwrap();
        assert_eq!(response.status, status);
        let bytes = response
            .body
            .map(|b| b.unwrap())
            .collect::<Vec<_>>()
            .await
            .concat();
        let out = String::from_utf8(bytes).unwrap();
        assert!(out.contains("https://chatgpt.com/settings/usage"));
        assert!(!out.contains("access-one"));
        assert!(!out.contains("refresh-one"));
        assert_eq!(client.requests.lock().unwrap().len(), 1);
    }
}
#[tokio::test]
async fn refresh_persists_rotated_credentials_once_before_returning_them() {
    let client = Client::new(vec![Reply::json(
        200,
        json!({"access_token":"access-two","refresh_token":"refresh-two","scope":auth::SCOPES,"expires_in":3600}),
    )]);
    let (_directory, provider) = configured(client.clone()).await;
    let old = saved(60_000);
    let one = provider.refresh(json!({"credentials":old})).await.unwrap();
    let two = provider.refresh(json!({"credentials":old})).await.unwrap();
    assert_eq!(one, two);
    let saved = provider.saved.lock().await;
    assert!(saved.iter().all(|value| value["refresh"] == "refresh-two"));
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn account_deletion_revokes_the_selected_private_snapshot_without_refresh() {
    let client = Client::new(vec![
        Reply::json(
            200,
            json!({"revocation_endpoint":format!("{}/oauth/revoke",auth::ISSUER)}),
        ),
        Reply::raw(200, ""),
    ]);
    let (_directory, provider) = configured(client.clone()).await;
    assert!(provider.supports_account_revocation());
    let mut context = context();
    context.credentials = Some(saved(-1));
    provider.revoke_account(context).await.unwrap();
    assert_eq!(client.form(1)["token"], "refresh-one");
    assert_eq!(client.form(1)["client_id"], "oaiapp_registered");
    assert_eq!(client.requests.lock().unwrap().len(), 2);
    assert!(provider.saved.lock().await.is_empty());
}

#[tokio::test]
async fn cancel_authorization_interrupts_an_already_waiting_callback() {
    let client = Client::new(vec![]);
    let (_directory, provider) = configured(client.clone()).await;
    provider
        .config
        .write()
        .unwrap()
        .as_mut()
        .unwrap()
        .endpoints
        .callback = "127.0.0.1:0".into();
    let start = provider
        .authorize(json!({"authMethodID":"oauth"}))
        .await
        .unwrap();
    let session = start["session"].clone();
    let callback = provider.callback(json!({"session":session}));
    let cancel = async {
        tokio::task::yield_now().await;
        provider
            .call("cancelAuthorization", json!({"session":session}))
            .await
            .unwrap();
    };
    let (result, _) = tokio::join!(callback, cancel);
    assert!(result.is_err());
    assert!(provider.flows.lock().await.is_empty());
    assert!(client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn public_live_codex_catalog_is_used_without_importing_another_accounts_credentials() {
    let client = Client::new(vec![
        Reply::json(
            200,
            json!({"models":[{"slug":"primary"},{"slug":"hidden","visibility":"hide"}]}),
        ),
        Reply::json(200, json!({"models":[{"slug":"primary"}]})),
    ]);
    let (_directory, provider) = configured(client).await;
    let mut public_context = context();
    public_context.public_model_catalogs.insert(
        "codex".into(),
        vec![
            json!({"id":"public-new","contextWindow":128000}),
            json!({"id":"hidden"}),
            json!({"id":"other/model"}),
        ],
    );
    public_context
        .public_model_catalogs
        .insert("openai".into(), vec![json!({"id":"api-key-only"})]);
    let out = provider.models(public_context.clone()).await.unwrap();
    assert_eq!(
        out["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["primary", "public-new"]
    );
    public_context
        .public_model_catalogs
        .insert("codex".into(), vec![]);
    assert_eq!(
        provider.models(public_context).await.unwrap()["models"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn an_openai_api_key_catalog_is_not_a_codex_supplement_when_no_subscription_catalog_exists() {
    let client = Client::new(vec![Reply::json(
        200,
        json!({"models":[{"slug":"primary"}]}),
    )]);
    let (_directory, provider) = configured(client).await;
    let mut public_context = context();
    let api_only = format!("api-key-only-{}", uuid::Uuid::new_v4());
    public_context
        .public_model_catalogs
        .insert("openai".into(), vec![json!({"id":api_only})]);
    let result = provider.models(public_context).await.unwrap();
    assert!(!result["models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|model| model["id"] == api_only));
    let manifest: Value = serde_json::from_str(include_str!("../nora-plugin.json")).unwrap();
    assert_eq!(
        manifest["contributes"]["provider"]["config"]["supplementalCatalogs"],
        json!([{"provider":"openai","api":"chatgpt-codex","as":"codex"}])
    );
}
