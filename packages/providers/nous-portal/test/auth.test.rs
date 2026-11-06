use super::*;
use crate::test_support::{access, jwt, Client, Reply};

fn response(lifetime: Option<Value>) -> Value {
    let mut body = json!({"access_token":access(Some("member"),3600_000),"refresh_token":"refresh-one","scope":SCOPE,"token_type":"Bearer"});
    if let Some(life) = lifetime {
        body["expires_in"] = life;
    }
    body
}
pub(crate) fn saved(lifetime: i64) -> Value {
    let mut auth = token(&response(Some(json!(3600))), None, now()).unwrap();
    auth["expires"] = json!(now() + lifetime);
    auth["access"] = json!(access(Some("member"), lifetime));
    auth
}
fn device_reply() -> Value {
    json!({"device_code":"private-device","user_code":"ABCD-1234","verification_uri":format!("{PORTAL}/device"),"expires_in":600,"interval":5})
}

#[tokio::test(start_paused = true)]
async fn device_authorization_uses_public_client_scope_and_minimum_poll_intervals() {
    let client = Client::new(vec![
        Reply::json(200, device_reply()),
        Reply::json(400, json!({"error":"authorization_pending"})),
        Reply::json(400, json!({"error":"slow_down"})),
        Reply::json(200, response(Some(json!(3600)))),
    ]);
    let started = tokio::time::Instant::now();
    let (reply, flow) = device(client.as_ref()).await.unwrap();
    assert_eq!(
        client.form(0),
        BTreeMap::from([
            ("client_id".into(), CLIENT.into()),
            ("scope".into(), SCOPE.into())
        ])
    );
    assert_eq!(reply["userCode"], "ABCD-1234");
    let (a, b) = tokio::join!(
        callback(&flow, client.clone()),
        callback(&flow, client.clone())
    );
    assert_eq!(a.unwrap(), b.unwrap());
    assert_eq!(
        tokio::time::Instant::now() - started,
        Duration::from_secs(20)
    );
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    drop(requests);
    let form = client.form(1);
    assert_eq!(
        form["grant_type"],
        "urn:ietf:params:oauth:grant-type:device_code"
    );
    assert_eq!(form["device_code"], "private-device");
    assert!(!form.contains_key("client_secret"));
}
#[tokio::test(start_paused = true)]
async fn omitted_interval_defaults_to_five_and_late_callback_does_not_restart_lifetime() {
    let mut reply = device_reply();
    reply.as_object_mut().unwrap().remove("interval");
    reply["expires_in"] = json!(8);
    let client = Client::new(vec![Reply::json(200, reply)]);
    let (_, flow) = device(client.as_ref()).await.unwrap();
    tokio::time::advance(Duration::from_secs(9)).await;
    assert!(callback(&flow, client.clone())
        .await
        .unwrap_err()
        .message
        .contains("expired_token"));
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}
#[tokio::test(start_paused = true)]
async fn retry_after_and_waf_backoff_stay_inside_original_monotonic_deadline() {
    let mut device_body = device_reply();
    device_body["expires_in"] = json!(24);
    let mut limited = Reply::raw(429, "<html>private vendor refusal</html>");
    limited.headers.insert("retry-after".into(), "15".into());
    let client = Client::new(vec![
        Reply::json(200, device_body),
        limited,
        Reply::raw(403, "<html>WAF</html>"),
    ]);
    let started = tokio::time::Instant::now();
    let (_, flow) = device(client.as_ref()).await.unwrap();
    let error = callback(&flow, client.clone()).await.unwrap_err();
    assert!(!error.message.contains("private"));
    assert!(tokio::time::Instant::now() - started <= Duration::from_secs(24));
}
#[tokio::test]
async fn verification_urls_and_malformed_device_replies_are_rejected() {
    for value in [
        "https://evil.test/device",
        "https://portal.nousresearch.com:444/device",
        "http://portal.nousresearch.com/device",
        "https://user@portal.nousresearch.com/device",
        "https://portal.nousresearch.com/device#fragment",
    ] {
        assert!(verification_url(value).is_err());
    }
    for field in ["device_code", "user_code", "expires_in", "interval"] {
        let mut bad = device_reply();
        bad[field] = json!(0);
        let client = Client::new(vec![Reply::json(200, bad)]);
        assert!(device(client.as_ref()).await.is_err());
    }
}
#[test]
fn token_lifetimes_use_the_earliest_valid_bound_and_a_stable_fallback() {
    let at = now();
    let no_exp = jwt(json!({"scope":SCOPE}));
    let fallback = token(
        &json!({"access_token":no_exp,"refresh_token":"r","scope":SCOPE}),
        None,
        at,
    )
    .unwrap();
    assert_eq!(fallback["expires"], at + 3600_000);
    let earlier = token(&response(Some(json!(300))), None, at).unwrap();
    assert_eq!(earlier["expires"], at + 300_000);
    for malformed in [Value::Null, json!("3600")] {
        let valid_jwt = token(&response(Some(malformed.clone())), None, at).unwrap();
        assert!(valid_jwt["expires"].as_i64().unwrap() > at + 3500_000);
        let broken=token(&json!({"access_token":jwt(json!({"scope":SCOPE})),"refresh_token":"r","scope":SCOPE,"expires_in":malformed}),None,at).unwrap();
        assert_eq!(broken["expires"], 1);
    }
    for malformed in [json!(-1), json!(0)] {
        let broken = token(&response(Some(malformed)), None, at).unwrap();
        if broken["expires"] != 1 {
            assert!(broken["expires"].as_i64().unwrap() <= at);
        }
    }
}
#[test]
fn scope_bearer_and_jwt_validation_never_turn_missing_subject_into_shared_identity() {
    for payload in [
        json!({"scope":"profile"}),
        json!({"scope":SCOPE,"exp":0}),
        json!({"scope":SCOPE,"exp":"bad"}),
    ] {
        let auth = token(
            &json!({"access_token":jwt(payload),"refresh_token":"r"}),
            None,
            now(),
        )
        .unwrap();
        assert!(!usable(&auth, now(), 0));
    }
    let without_subject = token(
        &json!({"access_token":jwt(json!({"scp":[SCOPE]})),"refresh_token":"r"}),
        None,
        now(),
    )
    .unwrap();
    assert!(usable(&without_subject, now(), 0));
    assert!(without_subject.get("accountId").is_none());
    assert!(without_subject.get("accountIdentity").is_none());
    let decoded = token(&response(Some(json!(3600))), None, now()).unwrap();
    assert_eq!(decoded["accountIdentity"]["issuer"], PORTAL);
    assert_eq!(decoded["accountIdentity"]["subject"], "member");
}
#[test]
fn inference_metadata_cannot_redirect_the_bearer_to_a_foreign_host() {
    for input in [
        "https://evil.test/v1",
        "https://inference-api.nousresearch.com:444/v1",
        "https://user@inference-api.nousresearch.com/v1",
        "https://inference-api.nousresearch.com/v1?secret=token",
    ] {
        assert_eq!(inference_url(input), INFERENCE);
    }
    assert_eq!(inference_url(WELCOME), WELCOME);
    assert_eq!(inference_url(&format!("{WELCOME}/")), WELCOME);
    for auth in [
        json!({}),
        json!({"refresh":"secret"}),
        json!({"refresh":"{}"}),
    ] {
        assert!(metadata(&auth).is_err());
    }
}
#[tokio::test]
async fn refresh_uses_only_the_refresh_header_and_single_flight_retains_rotated_successors() {
    let mut next = response(Some(json!(3600)));
    next["refresh_token"] = json!("refresh-two");
    let client = Client::new(vec![Reply::json(200, next)]);
    let book = Rotations::default();
    let old = saved(30_000);
    let (a, b) = tokio::join!(
        book.refresh(&old, client.clone(), false),
        book.refresh(&old, client.clone(), false)
    );
    let a = a.unwrap();
    assert_eq!(a, b.unwrap());
    assert_eq!(metadata(&a).unwrap().token, "refresh-two");
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    assert_eq!(
        client.requests.lock().unwrap()[0].headers["x-nous-refresh-token"],
        "refresh-one"
    );
    assert!(!client.form(0).contains_key("refresh_token"));
    assert_eq!(book.refresh(&old, client.clone(), false).await.unwrap(), a);
    assert_eq!(book.refresh(&old, client.clone(), true).await.unwrap(), a);
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn explicit_dead_grants_expire_but_unknown_401_403_and_429_keep_credentials() {
    for status in [400, 401, 403] {
        for code in ["invalid_grant", "invalid_token", "refresh_token_reused"] {
            let client = Client::new(vec![Reply::json(
                status,
                json!({"error":code,"error_description":"access-private refresh-private"}),
            )]);
            let error = Rotations::default()
                .refresh(&saved(-1), client, false)
                .await
                .unwrap_err();
            assert_eq!(error.code, "authentication_required");
            assert!(!error.message.contains("private"));
        }
    }
    for status in [401, 403, 429, 500] {
        let client = Client::new(vec![Reply::raw(status, "private raw body")]);
        let error = Rotations::default()
            .refresh(&saved(-1), client, false)
            .await
            .unwrap_err();
        assert_ne!(error.code, "authentication_required");
        assert!(!error.message.contains("private"));
    }
}
#[tokio::test]
async fn proactive_transient_failure_uses_still_valid_jwt_and_a_forced_retry_never_reuses_rejected_access(
) {
    let old = saved(60_000);
    let client = Client::new(vec![
        Reply::json(503, json!({})),
        Reply::json(503, json!({})),
    ]);
    let book = Rotations::default();
    assert_eq!(
        book.refresh(&old, client.clone(), false).await.unwrap(),
        old
    );
    assert_eq!(
        book.refresh(&old, client.clone(), false).await.unwrap(),
        old
    );
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    assert!(book.refresh(&old, client, true).await.is_err());
}
#[tokio::test]
async fn unusable_rotated_successor_is_returned_for_persistence_before_validation_and_not_redeemed_twice(
) {
    let old = saved(-1);
    let mut next = response(Some(json!(3600)));
    next["access_token"] = json!(jwt(json!({"scope":"profile"})));
    next["scope"] = json!("profile");
    next["refresh_token"] = json!("rotated-private");
    let client = Client::new(vec![Reply::json(200, next)]);
    let book = Rotations::default();
    let got = book.refresh(&old, client.clone(), false).await.unwrap();
    assert_eq!(got["expires"], 1);
    assert_eq!(metadata(&got).unwrap().token, "rotated-private");
    assert!(book.refresh(&old, client.clone(), false).await.is_err());
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}
#[tokio::test(start_paused = true)]
async fn canceling_one_waiter_does_not_abandon_a_rotation_needed_by_another() {
    let mut reply = Reply::json(200, response(Some(json!(3600))));
    reply.delay = Duration::from_secs(2);
    let client = Client::new(vec![reply]);
    let book = Rotations::default();
    let old = saved(-1);
    let a_book = book.clone();
    let a_client = client.clone();
    let a_old = old.clone();
    let a = tokio::spawn(async move { a_book.refresh(&a_old, a_client, false).await });
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    a.abort();
    let got = book.refresh(&old, client.clone(), false).await.unwrap();
    assert!(usable(&got, now(), LEAD));
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn successful_oauth_response_with_null_error_is_not_misclassified_as_a_failure() {
    let mut device_body = device_reply();
    device_body["error"] = Value::Null;
    let mut token_body = response(Some(json!(3600)));
    token_body["error"] = Value::Null;
    let client = Client::new(vec![Reply::json(200, device_body)]);
    client.push(200, token_body);
    let (_, flow) = device(client.as_ref()).await.unwrap();
    assert!(callback(&flow, client).await.is_ok());
}
