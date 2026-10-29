use super::*;
use crate::tests::{configured, credentials, envelope, Client, Sink};
fn jwt(value: Value) -> String {
    format!(
        "header.{}.signature",
        URL_SAFE_NO_PAD.encode(value.to_string())
    )
}

#[test]
fn tokens_trim_bearer_and_prefer_jwt_expiry_with_skew_without_guessing_identity_from_a_name() {
    let at = 1_000_000_000;
    let access = jwt(json!({"exp":2_000_000_000}));
    let value = tokens(
        &json!({"access_token":format!("Bearer Bearer {access}"),"refresh_token":"Bearer new"}),
        &Value::Null,
        Site::China,
        at,
    )
    .unwrap();
    assert_eq!(value["access"], access);
    assert_eq!(value["refresh"], "new");
    assert_eq!(value["expires"], 1_999_999_400_000u64);
    assert!(value.get("accountIdentity").is_none());
    assert!(tokens(
        &json!({"access_token":"access"}),
        &Value::Null,
        Site::China,
        at
    )
    .is_err());
}

#[tokio::test]
async fn phone_login_masks_the_number_sends_numeric_code_and_uses_real_profile_identity() {
    let client = Client::new(vec![
        envelope(json!({})),
        envelope(
            json!({"access_token":"access-two","refresh_token":"refresh-two","user_id":"uid"}),
        ),
        envelope(json!({"user_name":"Tester","user_phone":"13800138000","user_id":"uid"})),
    ]);
    let provider = configured(Site::China, client.clone());
    let authorization = provider
        .authorize(json!({"method":"oauth","inputs":{"phone":"13800138000"}}))
        .await
        .unwrap();
    assert_eq!(authorization["method"], "code");
    assert!(!authorization["instructions"]
        .as_str()
        .unwrap()
        .contains("13800138000"));
    let completed = provider
        .callback(json!({"session":authorization["session"],"code":"001234"}))
        .await
        .unwrap();
    assert_eq!(
        completed["credentials"]["accountIdentity"]["subject"],
        "uid"
    );
    assert_eq!(completed["credentials"]["name"], "Tester");
    let requests = client.requests.lock().unwrap();
    assert!(requests
        .iter()
        .all(|r| r.url.starts_with(Site::China.host())));
    assert_eq!(
        serde_json::from_slice::<Value>(requests[1].body.as_ref().unwrap()).unwrap()["code"],
        1234
    );
}

#[tokio::test]
async fn invalid_phone_and_cancelled_or_expired_sessions_do_not_send_a_login() {
    let client = Client::new(vec![envelope(json!({}))]);
    let provider = configured(Site::China, client.clone());
    for phone in ["+8613800138000", "123", "1380013800a"] {
        assert!(provider
            .authorize(json!({"inputs":{"phone":phone}}))
            .await
            .is_err());
    }
    assert!(client.requests.lock().unwrap().is_empty());
    let auth = provider
        .authorize(json!({"inputs":{"phone":"13800138000"}}))
        .await
        .unwrap();
    assert_eq!(
        cancel(&provider, &json!({"session":auth["session"]}))
            .await
            .unwrap()["cancelled"],
        true
    );
    assert!(provider
        .callback(json!({"session":auth["session"],"code":"123456"}))
        .await
        .is_err());
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn rotation_uses_jwt_origin_and_agent_fallback_and_is_saved_before_any_later_request() {
    let client = Client::new(vec![
        (200, json!({"code":400002})),
        envelope(
            json!({"access_token":"rotated","refresh_token":"rotated-refresh","expires_in":3600}),
        ),
    ]);
    let mut provider = configured(Site::China, client.clone());
    let sink = Arc::new(Sink::default());
    provider.sink = sink.clone();
    let mut old = credentials(Site::China);
    old["refresh"] = json!(jwt(json!({"source_id":"agent","device_id":"device-one"})));
    let result = provider.refresh(json!({"credentials":old})).await.unwrap();
    assert_eq!(result["credentials"]["refresh"], "rotated-refresh");
    assert_eq!(sink.writes.lock().unwrap().len(), 1);
    let requests = client.requests.lock().unwrap();
    assert!(requests[0].url.ends_with("/userapi/v1/refresh"));
    assert!(requests[1].url.ends_with("/userapi/v1/agent-refresh"));
    assert!(requests
        .iter()
        .all(|r| r.url.starts_with(Site::China.host())));
    assert_eq!(
        serde_json::from_slice::<Value>(requests[0].body.as_ref().unwrap()).unwrap()["device_id"],
        "device-one"
    );
}

#[tokio::test]
async fn a_refresh_write_failure_prevents_the_rotated_pair_from_being_used_or_reported_as_saved() {
    let client = Client::new(vec![envelope(
        json!({"access_token":"new-secret","refresh_token":"new-refresh"}),
    )]);
    let mut provider = configured(Site::International, client.clone());
    let sink = Arc::new(Sink::default());
    sink.reject.store(true, std::sync::atomic::Ordering::SeqCst);
    provider.sink = sink.clone();
    let mut old = credentials(Site::International);
    old["expires"] = json!(0);
    let mut context = CallContext::default();
    context.credentials = Some(old);
    let error = provider.usage(context).await.unwrap_err().to_string();
    assert!(!error.contains("new-secret"));
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    assert_eq!(sink.writes.lock().unwrap()[0]["refresh"], "new-refresh");
}

#[tokio::test]
async fn web_refresh_tries_the_other_host_only_for_wrong_side_and_plain_401_is_expired() {
    let client = Client::new(vec![
        (200, json!({"code":400000})),
        envelope(json!({"access_token":"new","refresh_token":"new-r"})),
    ]);
    renew(
        client.as_ref(),
        Site::International,
        &credentials(Site::International),
    )
    .await
    .unwrap();
    assert!(client.requests.lock().unwrap()[0]
        .url
        .starts_with(Site::International.host()));
    assert!(client.requests.lock().unwrap()[1]
        .url
        .starts_with(Site::China.host()));
    let client = Client::new(vec![(401, json!({"raw":"Bearer sensitive-token"}))]);
    let error = renew(
        client.as_ref(),
        Site::International,
        &credentials(Site::International),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(!error.contains("sensitive-token"));
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}
