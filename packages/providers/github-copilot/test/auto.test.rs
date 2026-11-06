use super::*;
use async_trait::async_trait;
use futures::stream;
use nora_ai::{HttpClientError, HttpStreamingResponse};
use nora_provider_plugin_sdk::model_resolution::PreviousFailure;
use nora_provider_plugin_sdk::CallContext;
use std::{collections::VecDeque, sync::Mutex as SyncMutex};

struct Client {
    requests: SyncMutex<Vec<HttpRequest>>,
    replies: SyncMutex<VecDeque<(u16, Value)>>,
}

#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        let (status, body) = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected HTTP request");
        Ok(HttpStreamingResponse {
            status,
            headers: BTreeMap::new(),
            body: Box::pin(stream::once(async move {
                Ok(serde_json::to_vec(&body).unwrap().into())
            })),
        })
    }
}

fn client(replies: Vec<(u16, Value)>) -> Client {
    Client {
        requests: SyncMutex::new(Vec::new()),
        replies: SyncMutex::new(replies.into()),
    }
}

fn definition() -> Definition {
    let mut definition = Definition::from_fields(json!({
        "id":"github-copilot", "name":"GitHub Copilot", "api":"openai-completions",
        "baseURL":"https://api.githubcopilot.com",
        "config":{"keyProvider":"github-copilot", "oauthProvider":"github-copilot"}
    }))
    .unwrap();
    definition.adapter = Arc::new(super::super::Copilot::default());
    definition
}

fn auth(account_token: &str) -> ResolvedProviderAuth {
    ResolvedProviderAuth {
        token: Some("private-chat-session".into()),
        base_url: Some("https://api.githubcopilot.com".into()),
        credential_provider_id: Some("github-copilot".into()),
        headers: copilot_client::api_headers(),
        metadata: BTreeMap::from([(
            "githubOAuthToken".into(),
            nora_ai::JsonValue::string(account_token),
        )]),
        ..Default::default()
    }
}

fn request_params() -> ResolveModelRequest {
    ResolveModelRequest {
        context: CallContext {
            account: "stored-account".into(),
            credentials: Some(
                json!({"type":"oauth", "access":"private-chat-session", "refresh":"private-github-token"}),
            ),
            ..Default::default()
        },
        model: serde_json::to_value(Model::new("auto", "github-copilot-chat", "github-copilot"))
            .unwrap(),
        prompt: "Write a Rust function that reverses a list.".into(),
        session_id: Some("conversation".into()),
        force: false,
        previous_failure: None,
    }
}

fn selected(model: &str, api: &str, token: &str) -> Value {
    json!({
        "session_token":token,
        "selected_model":{"id":model, "supported_endpoints":[api],
            "capabilities":{"type":"chat", "limits":{"max_context_window_tokens":128000}}},
        "expires_at":nora_ai::oauth::now_millis()/1000+86400
    })
}

fn model(id: &str, api: &str) -> Value {
    json!({"id":id,"model_picker_enabled":true,"policy":{"state":"enabled"},
        "supported_endpoints":[api],"capabilities":{"type":"chat"}})
}

fn retry(result: &ModelResolution, status: u16, body: &str) -> ResolveModelRequest {
    let mut request = request_params();
    request.force = true;
    request.previous_failure = Some(PreviousFailure {
        status,
        body: body.into(),
        selection_id: result.selection_id.clone(),
    });
    request
}

#[tokio::test]
async fn auto_is_resolved_before_encoding_and_keeps_the_session_private() {
    let client = client(vec![(
        200,
        selected(
            "server-picked-current",
            "/responses",
            "private-auto-session",
        ),
    )]);
    let resolver = Resolver::default();
    let result = resolver
        .resolve(&definition(), request_params(), &auth("account-a"), &client)
        .await
        .unwrap()
        .unwrap();
    let selected: Model = serde_json::from_value(result.model.clone()).unwrap();
    assert_eq!(selected.id, "server-picked-current");
    assert_eq!(selected.api, "github-copilot-openai-responses");
    assert_eq!(selected.context_window, 128000);
    assert_eq!(selected.base_url, "https://api.githubcopilot.com");
    assert_eq!(
        result.headers["copilot-session-token"],
        "private-auto-session"
    );
    assert!(result.selection_id.is_some());
    assert!(!result.model.to_string().contains("private-"));
    assert!(!format!("{result:?}").contains("private-"));
    assert!(!format!("{resolver:?}").contains("private-"));
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, "https://api.githubcopilot.com/auto");
    assert_eq!(requests[0].headers["x-github-api-version"], "2026-08-01");
    assert_eq!(requests[0].headers["editor-version"], "vscode/1.140.0");
    assert_eq!(
        requests[0].headers["authorization"],
        "Bearer private-chat-session"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(requests[0].body.as_ref().unwrap()).unwrap(),
        json!({
            "prompt":"Write a Rust function that reverses a list.", "tier":"balance"
        })
    );
}

#[tokio::test]
async fn cached_sessions_are_account_specific_and_expire_within_one_hour() {
    let client = client(vec![
        (200, selected("first", "/responses", "first-session")),
        (200, selected("second", "/v1/messages", "second-session")),
    ]);
    let resolver = Resolver::default();
    let first = resolver
        .resolve(&definition(), request_params(), &auth("account-a"), &client)
        .await
        .unwrap()
        .unwrap();
    let again = resolver
        .resolve(&definition(), request_params(), &auth("account-a"), &client)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.selection_id, first.selection_id);
    let other = resolver
        .resolve(&definition(), request_params(), &auth("account-b"), &client)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(other.model["id"], "second");
    assert_eq!(other.model["api"], "github-copilot-anthropic-messages");
    assert_eq!(other.headers["copilot-session-token"], "second-session");
    assert_eq!(client.requests.lock().unwrap().len(), 2);
    for account in resolver.accounts.lock().await.values() {
        let state = account.lock().await;
        assert!(state.current.as_ref().unwrap().expires <= Instant::now() + REUSE_FOR);
    }
}

#[tokio::test]
async fn cli_accounts_keep_their_own_headers_on_auto_requests() {
    let client = client(vec![(
        200,
        selected("picked", "/chat/completions", "session"),
    )]);
    let mut auth = auth("account-a");
    auth.headers = copilot_client::cli_headers();
    Resolver::default()
        .resolve(&definition(), request_params(), &auth, &client)
        .await
        .unwrap();
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        requests[0].headers["copilot-integration-id"],
        "copilot-developer-cli"
    );
    assert_eq!(requests[0].headers["user-agent"], "copilot/1.0.88");
    assert_eq!(requests[0].headers["x-github-api-version"], "2026-08-01");
}

#[tokio::test]
async fn legacy_sessions_use_the_selected_models_actual_protocol() {
    let client = client(vec![
        (404, json!({})),
        (
            200,
            json!({"session_token":"legacy-session", "selected_model":"picked", "expires_at":nora_ai::oauth::now_millis()/1000+3600}),
        ),
        (200, json!({"data":[model("picked", "/v1/messages")]})),
    ]);
    let result = Resolver::default()
        .resolve(&definition(), request_params(), &auth("account-a"), &client)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.model["id"], "picked");
    assert_eq!(result.model["api"], "github-copilot-anthropic-messages");
    assert_eq!(result.headers["copilot-session-token"], "legacy-session");
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        requests[1].url,
        "https://api.githubcopilot.com/models/session"
    );
    assert_eq!(requests[1].headers["x-github-api-version"], "2025-10-01");
    assert_eq!(
        serde_json::from_slice::<Value>(requests[1].body.as_ref().unwrap()).unwrap(),
        json!({"auto_mode":{"model_hints":["auto"]}})
    );
}

#[tokio::test]
async fn legacy_available_models_choose_a_known_api_before_an_unknown_one() {
    let client = client(vec![
        (500, json!({})),
        (
            200,
            json!({"session_token":"legacy-session", "available_models":["unknown", "known"]}),
        ),
        (200, json!({"data":[model("known", "/responses")]})),
    ]);
    let result = Resolver::default()
        .resolve(&definition(), request_params(), &auth("account-a"), &client)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.model["id"], "known");
    assert_eq!(result.model["api"], "github-copilot-openai-responses");
    assert_eq!(client.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn accounts_without_auto_use_a_live_eligible_fallback_without_a_fake_session() {
    let mut fallback = model("base", "/chat/completions");
    fallback["is_chat_fallback"] = json!(true);
    let mut disabled = model("disabled", "/responses");
    disabled["policy"]["state"] = json!("disabled");
    let client = client(vec![
        (404, json!({})),
        (404, json!({})),
        (
            200,
            json!({"data":[model("premium-first", "/responses"), disabled, fallback]}),
        ),
    ]);
    let result = Resolver::default()
        .resolve(&definition(), request_params(), &auth("account-a"), &client)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.model["id"], "base");
    assert_eq!(result.model["api"], "github-copilot-chat");
    assert!(result.headers.is_empty());
}

#[tokio::test]
async fn a_session_no_longer_accepted_is_renewed_without_blacklisting_its_model() {
    let client = client(vec![
        (200, selected("same-model", "/responses", "old-session")),
        (200, selected("same-model", "/responses", "new-session")),
    ]);
    let resolver = Resolver::default();
    let first = resolver
        .resolve(&definition(), request_params(), &auth("account-a"), &client)
        .await
        .unwrap()
        .unwrap();
    let renewed = resolver
        .resolve(
            &definition(),
            retry(&first, 400, "Requested model not available for session"),
            &auth("account-a"),
            &client,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(renewed.model["id"], "same-model");
    assert_eq!(renewed.headers["copilot-session-token"], "new-session");
    assert_ne!(renewed.selection_id, first.selection_id);
    let stale = resolver
        .resolve(
            &definition(),
            retry(&first, 400, "Requested model not available for session"),
            &auth("account-a"),
            &client,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stale.selection_id, renewed.selection_id);
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_model_the_account_cannot_use_is_avoided_even_if_auto_picks_it_again() {
    let client = client(vec![
        (200, selected("refused", "/responses", "old-session")),
        (200, selected("refused", "/responses", "wrong-again")),
        (
            200,
            json!({"data":[model("refused", "/responses"), model("available", "/v1/messages")]}),
        ),
    ]);
    let resolver = Resolver::default();
    let first = resolver
        .resolve(&definition(), request_params(), &auth("account-a"), &client)
        .await
        .unwrap()
        .unwrap();
    let retry = retry(&first, 400, r#"{"error":{"code":"model_not_supported"}}"#);
    let result = resolver
        .resolve(&definition(), retry, &auth("account-a"), &client)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.model["id"], "available");
    assert_eq!(result.model["api"], "github-copilot-anthropic-messages");
    assert!(result.headers.is_empty());
    assert_eq!(client.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn auto_and_the_manual_picker_share_the_same_accounts_refused_models() {
    use nora_provider::ProviderAdapter;
    let provider = super::super::Copilot::default();
    let params = request_params();
    provider.observe_response_error(
        params.context.credentials.as_ref().unwrap(),
        "manually-refused",
        400,
        b"The requested model is not supported.",
    );
    let client = client(vec![
        (200, selected("manually-refused", "/responses", "session")),
        (
            200,
            json!({"data":[model("manually-refused", "/responses"), model("available", "/v1/messages")]}),
        ),
    ]);
    let result = provider
        .resolve_model(&definition(), params, &auth("account-a"), &client)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.model["id"], "available");
    assert!(result.headers.is_empty());
}

#[tokio::test]
async fn unrelated_errors_and_other_accounts_selections_are_never_retried() {
    let client = client(vec![(200, selected("first", "/responses", "session"))]);
    let resolver = Resolver::default();
    let first = resolver
        .resolve(&definition(), request_params(), &auth("account-a"), &client)
        .await
        .unwrap()
        .unwrap();
    for (status, body) in [
        (400, "unsupported_api_for_model"),
        (401, "model_not_supported"),
        (429, "not available for session"),
    ] {
        assert!(resolver
            .resolve(
                &definition(),
                retry(&first, status, body),
                &auth("account-a"),
                &client
            )
            .await
            .unwrap()
            .is_none());
    }
    let mut unknown = retry(&first, 400, "requested model is not supported");
    unknown.previous_failure.as_mut().unwrap().selection_id = Some("unknown".into());
    assert!(resolver
        .resolve(&definition(), unknown, &auth("account-a"), &client)
        .await
        .unwrap()
        .is_none());
    assert!(resolver
        .resolve(
            &definition(),
            retry(&first, 400, "requested model is not supported"),
            &auth("account-b"),
            &client
        )
        .await
        .unwrap()
        .is_none());
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn ordinary_models_and_api_key_accounts_do_not_start_auto() {
    let client = client(Vec::new());
    let resolver = Resolver::default();
    let mut plain = request_params();
    plain.model["id"] = json!("ordinary-model");
    assert!(resolver
        .resolve(&definition(), plain, &auth("account-a"), &client)
        .await
        .unwrap()
        .is_none());
    let mut api = request_params();
    api.context.credentials = Some(json!({"type":"apiKey", "access":"key"}));
    assert!(resolver
        .resolve(&definition(), api, &auth("account-a"), &client)
        .await
        .unwrap()
        .is_none());
    assert!(client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unsupported_selected_endpoints_and_expired_sessions_are_not_invented_as_working() {
    for body in [selected("websocket-only", "ws:/responses", "session"), {
        let mut body = selected("expired", "/responses", "private-session");
        body["expires_at"] = json!(nora_ai::oauth::now_millis() / 1000 - 1);
        body
    }] {
        let client = client(vec![(200, body)]);
        let error = Resolver::default()
            .resolve(&definition(), request_params(), &auth("account-a"), &client)
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("private-"));
        assert_eq!(client.requests.lock().unwrap().len(), 1);
    }
}
