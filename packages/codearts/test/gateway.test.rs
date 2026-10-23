use super::*;
use crate::provider::tests::{configured, credentials, Client};
use base64::Engine;
use futures::StreamExt;
use nora_provider_plugin_sdk::ProviderPlugin;
use std::collections::BTreeSet;

pub(super) fn request(credentials: &Value, body: Value) -> FetchRequest {
    serde_json::from_value(json!({"account":"member","credentials":encode(credentials.clone()).unwrap(),"url":format!("{BASE}/chat/completions"),"method":"POST","headers":{"authorization":"Bearer attacker-controlled","x-security-token":"other-account"},"body":base64::engine::general_purpose::STANDARD.encode(body.to_string())})).unwrap()
}
async fn prime(provider: &CodeArts, credentials: &Value, benefit: bool) {
    let account = provider.account(credentials).await.unwrap();
    let mut state = account.models.lock().await;
    state.catalog = Some(json!({}));
    state.updated = now();
    state.benefit = Some(if benefit {
        BTreeSet::from(["glm-5.3".into()])
    } else {
        BTreeSet::new()
    });
    state.regular = Some(BTreeSet::from(["glm-5.3".into()]));
}
pub(super) async fn collect(mut response: FetchResponse) -> Vec<u8> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.body.next().await {
        bytes.extend(chunk.unwrap());
    }
    bytes
}
fn answer() -> Value {
    json!({"raw":"data: {\"id\":\"answer\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"})
}

#[test]
fn prepare_preserves_caller_thinking_and_only_caps_real_output_settings() {
    let mut body = json!({"model":"codearts/GLM-5.2","messages":[],"thinking":{"type":"disabled"},"max_tokens":131072,"stream":false});
    assert_eq!(prepare(&mut body).unwrap(), (true, false));
    assert_eq!(body["model"], "GLM-5.2");
    assert_eq!(body["thinking"]["type"], "disabled");
    assert_eq!(body["max_tokens"], 65536);
    assert_eq!(body["stream"], true);
}

#[tokio::test]
async fn signed_retry_removes_only_plugin_injected_thinking_and_keeps_account_headers() {
    let client = Client::new(vec![
        (
            400,
            json!({"error":{"code":"parameter","message":"thinking is unsupported"}}),
        ),
        (200, answer()),
    ]);
    let provider = configured(client.clone());
    let credentials = credentials();
    prime(&provider, &credentials, false).await;
    let response = fetch(
        &provider,
        request(
            &credentials,
            json!({"model":"glm-5.3","messages":[{"role":"user","content":"hello"}],"stream":true}),
        ),
    )
    .await
    .unwrap();
    assert_eq!(response.status, 200);
    assert!(String::from_utf8(collect(response).await)
        .unwrap()
        .contains("hello"));
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        serde_json::from_slice::<Value>(requests[0].body.as_ref().unwrap())
            .unwrap()
            .get("thinking")
            .is_some()
    );
    assert!(
        serde_json::from_slice::<Value>(requests[1].body.as_ref().unwrap())
            .unwrap()
            .get("thinking")
            .is_none()
    );
    assert_eq!(
        requests[0].headers["session-id"],
        requests[1].headers["session-id"]
    );
    assert_ne!(
        requests[0].headers["x-ot-trace-id"],
        requests[1].headers["x-ot-trace-id"]
    );
    assert_eq!(requests[0].headers["x-security-token"], "ST");
    assert!(requests[0].headers["authorization"].starts_with("SDK-HMAC-SHA256 Access=AK,"));
}

#[tokio::test]
async fn benefit_refusal_retries_the_same_builtin_model_without_free_billing_header() {
    let client = Client::new(vec![
        (
            200,
            json!({"raw":"data: {\"error_code\":\"InferHub.4004.200\",\"error_msg\":\"benefit not found\"}\n\n"}),
        ),
        (200, answer()),
    ]);
    let provider = configured(client.clone());
    let credentials = credentials();
    prime(&provider, &credentials, true).await;
    assert_eq!(
        fetch(
            &provider,
            request(
                &credentials,
                json!({"model":"glm-5.3","messages":[],"stream":true})
            )
        )
        .await
        .unwrap()
        .status,
        200
    );
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].headers["maas_type"], "benefit");
    assert!(!requests[1].headers.contains_key("maas_type"));
    drop(requests);
    assert!(provider
        .account(&credentials)
        .await
        .unwrap()
        .models
        .lock()
        .await
        .refused
        .contains("glm-5.3"));
}

#[tokio::test]
async fn embedded_errors_become_real_statuses_and_stale_keys_are_left_to_host_refresh() {
    for (code, status) in [
        ("InferHub.4291.200", 429),
        ("InferHub.4004.200", 400),
        ("APIG.0602", 401),
    ] {
        let client = Client::new(vec![(
            200,
            json!({"raw":format!("data: {}\n\n",json!({"error_code":code,"error_msg":"upstream rejected"}))}),
        )]);
        let provider = configured(client.clone());
        let credentials = credentials();
        prime(&provider, &credentials, false).await;
        let response = fetch(
            &provider,
            request(
                &credentials,
                json!({"model":"glm-5.3","messages":[],"stream":true}),
            ),
        )
        .await
        .unwrap();
        assert_eq!(response.status, status);
        let value: Value = serde_json::from_slice(&collect(response).await).unwrap();
        assert!(value.get("error").is_some());
        assert_eq!(client.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn forbidden_http_body_is_preserved_and_sign_in_expired_is_not_overwritten() {
    let original = json!({"error":{"code":"permission_denied","message":"forbidden"}});
    let client = Client::new(vec![(403, original.clone())]);
    let provider = configured(client);
    let credentials = credentials();
    prime(&provider, &credentials, false).await;
    let response = fetch(
        &provider,
        request(
            &credentials,
            json!({"model":"glm-5.3","messages":[],"stream":true}),
        ),
    )
    .await
    .unwrap();
    assert_eq!(response.status, 403);
    assert_eq!(response.headers["x-magpie-sign-in"], "expired");
    assert_eq!(
        serde_json::from_slice::<Value>(&collect(response).await).unwrap(),
        original
    );
}

#[tokio::test]
async fn a_promoted_queue_is_polled_with_the_same_account_before_resending() {
    let client = Client::new(vec![
        (
            429,
            json!({"raw":"","headers":{"queue-status":"queued","trace-id":"trace/one","retry-after":"0"}}),
        ),
        (200, json!({"status":"promoted"})),
        (200, answer()),
    ]);
    let provider = configured(client.clone());
    let credentials = credentials();
    prime(&provider, &credentials, false).await;
    let response = fetch(
        &provider,
        request(
            &credentials,
            json!({"model":"glm-5.3","messages":[],"stream":true}),
        ),
    )
    .await
    .unwrap();
    collect(response).await;
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].method, HttpMethod::Get);
    assert!(requests[1].url.ends_with("trace%2Fone"));
    assert_eq!(
        requests[1].headers["x-ot-session-id"],
        requests[0].headers["x-ot-session-id"]
    );
    assert_eq!(requests[2].method, HttpMethod::Post);
}

#[tokio::test]
async fn cancelling_a_queued_request_deletes_only_its_trace_and_releases_its_permit() {
    let client = Client::new(vec![
        (
            429,
            json!({"raw":"","headers":{"queue-status":"queued","trace-id":"cancel-me","retry-after":"30"}}),
        ),
        (204, json!({})),
    ]);
    let provider = Arc::new(configured(client.clone()));
    let credentials = credentials();
    prime(&provider, &credentials, false).await;
    let task = tokio::spawn({
        let provider = provider.clone();
        let credentials = credentials.clone();
        async move {
            fetch(
                &provider,
                request(
                    &credentials,
                    json!({"model":"glm-5.3","messages":[],"stream":true}),
                ),
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if !client.requests.lock().unwrap().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    task.abort();
    let _ = task.await;
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if client.requests.lock().unwrap().len() == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[1].method, HttpMethod::Delete);
    assert!(requests[1].url.ends_with("cancel-me"));
    assert_eq!(requests[1].headers["x-security-token"], "ST");
    assert_eq!(
        requests[1].headers["x-ot-session-id"],
        requests[0].headers["x-ot-session-id"]
    );
    drop(requests);
    assert_eq!(
        provider
            .account(&credentials)
            .await
            .unwrap()
            .gate
            .available_permits(),
        3
    );
}

#[tokio::test]
async fn only_three_chats_can_hold_an_accounts_permits_until_their_bodies_end() {
    let pending = json!({"raw":"data: {\"choices\":[{\"delta\":{\"content\":\"started\"}}]}\n\n","pending":true});
    let client = Client::new(vec![
        (200, pending.clone()),
        (200, pending.clone()),
        (200, pending),
        (200, answer()),
    ]);
    let provider = configured(client.clone());
    let credentials = credentials();
    prime(&provider, &credentials, false).await;
    let mut open = Vec::new();
    for _ in 0..3 {
        open.push(
            fetch(
                &provider,
                request(
                    &credentials,
                    json!({"model":"glm-5.3","messages":[],"stream":true}),
                ),
            )
            .await
            .unwrap(),
        );
    }
    let mut fourth = Box::pin(fetch(
        &provider,
        request(
            &credentials,
            json!({"model":"glm-5.3","messages":[],"stream":true}),
        ),
    ));
    assert!(futures::poll!(fourth.as_mut()).is_pending());
    let mut cancelled = Box::pin(fetch(
        &provider,
        request(
            &credentials,
            json!({"model":"glm-5.3","messages":[],"stream":true}),
        ),
    ));
    assert!(futures::poll!(cancelled.as_mut()).is_pending());
    drop(cancelled);
    assert_eq!(client.requests.lock().unwrap().len(), 3);
    assert_eq!(
        provider
            .account(&credentials)
            .await
            .unwrap()
            .gate
            .available_permits(),
        0
    );

    drop(open.remove(0));
    let response = tokio::time::timeout(Duration::from_secs(1), fourth)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(client.requests.lock().unwrap().len(), 4);
    assert!(String::from_utf8(collect(response).await)
        .unwrap()
        .contains("hello"));
    assert_eq!(
        provider
            .account(&credentials)
            .await
            .unwrap()
            .gate
            .available_permits(),
        1
    );
    drop(open);
    assert_eq!(
        provider
            .account(&credentials)
            .await
            .unwrap()
            .gate
            .available_permits(),
        3
    );
}

#[tokio::test]
async fn max_inflight_can_lower_the_limit_but_never_exceed_three_sessions() {
    for (configured_limit, expected) in [(10, 3), (1, 1), (0, 3)] {
        let pending = json!({"raw":"data: {\"choices\":[{\"delta\":{\"content\":\"started\"}}]}\n\n","pending":true});
        let client = Client::new(vec![(200, pending); expected + 1]);
        let provider = configured(client.clone());
        let mut manifest: Value =
            serde_json::from_str(include_str!("../nora-provider.json")).unwrap();
        manifest["config"]["maxInflight"] = json!(configured_limit);
        provider
            .initialize(nora_provider_plugin_sdk::InitializeParams {
                api_version: 1,
                plugin_id: "codearts".into(),
                plugin_directory: std::env::temp_dir(),
                data_directory: std::env::temp_dir(),
                manifest,
            })
            .await
            .unwrap();
        let credentials = credentials();
        prime(&provider, &credentials, false).await;
        let mut open = Vec::new();
        for _ in 0..expected {
            open.push(
                fetch(
                    &provider,
                    request(
                        &credentials,
                        json!({"model":"glm-5.3","messages":[],"stream":true}),
                    ),
                )
                .await
                .unwrap(),
            );
        }
        let mut waiting = Box::pin(fetch(
            &provider,
            request(
                &credentials,
                json!({"model":"glm-5.3","messages":[],"stream":true}),
            ),
        ));
        assert!(futures::poll!(waiting.as_mut()).is_pending());
        assert_eq!(client.requests.lock().unwrap().len(), expected);
        drop(open.remove(0));
        let admitted = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(client.requests.lock().unwrap().len(), expected + 1);
        drop(admitted);
        drop(open);
        assert_eq!(
            provider
                .account(&credentials)
                .await
                .unwrap()
                .gate
                .available_permits(),
            expected
        );
    }
}

#[tokio::test]
async fn an_expired_queue_budget_deletes_the_trace_without_resending_the_chat() {
    for polling in [false, true] {
        let replies = if polling {
            vec![
                (200, json!({"status":"queued","pending":true})),
                (204, json!({})),
            ]
        } else {
            vec![(204, json!({}))]
        };
        let client = Client::new(replies);
        let credentials = credentials();
        let promoted = wait_in_queue(
            client.clone(),
            credentials.clone(),
            "budget/trace".into(),
            if polling { 0.0 } else { 30.0 },
            tokio::time::Instant::now() + Duration::from_millis(20),
        )
        .await
        .unwrap();
        assert!(!promoted);
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if client
                    .requests
                    .lock()
                    .unwrap()
                    .last()
                    .is_some_and(|request| request.method == HttpMethod::Delete)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), if polling { 2 } else { 1 });
        if polling {
            assert_eq!(requests[0].method, HttpMethod::Get);
        }
        let deleted = requests.last().unwrap();
        assert_eq!(deleted.method, HttpMethod::Delete);
        assert!(deleted.url.ends_with("budget%2Ftrace"));
        assert_eq!(deleted.headers["x-security-token"], "ST");
        assert_eq!(deleted.headers["x-ot-session-id"], session(&credentials).0);
        assert!(!requests
            .iter()
            .any(|request| request.method == HttpMethod::Post));
    }
}

#[tokio::test]
async fn a_caller_cannot_send_account_credentials_to_an_overridden_endpoint() {
    let client = Client::new(vec![]);
    let provider = configured(client.clone());
    let credentials = credentials();
    let mut request = request(&credentials, json!({"model":"glm-5.3","messages":[]}));
    request.url = "https://attacker.example/chat/completions".into();
    assert!(fetch(&provider, request).await.is_err());
    assert!(client.requests.lock().unwrap().is_empty());
}
