use super::*;
use base64::Engine;

/// A JWT whose claims are these, unsigned.
pub(crate) fn jwt(claims: Value) -> String {
    let encode =
        |value: &Value| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string());
    format!(
        "{}.{}.sig",
        encode(&json!({"alg": "none"})),
        encode(&claims)
    )
}

#[test]
fn a_sign_in_is_named_by_its_email_else_its_user_id() {
    let tokens = json!({
        "access_token": "a1", "refresh_token": "r1", "expires_in": 3600,
        "id_token": jwt(json!({"email": "ada@example.test", "sub": "u-1"}))
    });
    let saved = credentials(&Value::Null, &tokens).unwrap();
    assert_eq!(saved["refresh"], "r1");
    assert_eq!(saved["userID"], "u-1");
    assert!(saved["expires"].as_u64().is_some());
    assert_eq!(account(saved).name, "ada@example.test");

    let through_x = json!({"access_token": "a1", "id_token": jwt(json!({"sub": "u-2"}))});
    assert_eq!(
        account(credentials(&Value::Null, &through_x).unwrap()).name,
        "u-2"
    );
}

#[test]
fn a_renewal_keeps_what_it_leaves_out() {
    let saved = json!({"access": "a1", "refresh": "r1", "email": "ada@example.test"});
    let next = credentials(&saved, &json!({"access_token": "a2", "expires_in": 60})).unwrap();
    assert_eq!(next["access"], "a2");
    assert_eq!(next["refresh"], "r1");
    assert_eq!(next["email"], "ada@example.test");
    assert!(ending(&next, Duration::from_secs(300)));
    assert!(!ending(
        &json!({"expires": 4_000_000_000u64}),
        Duration::from_secs(300)
    ));
}

#[test]
fn no_access_token_is_no_sign_in() {
    assert!(credentials(&Value::Null, &json!({"error": "invalid_grant"})).is_err());
}

#[test]
fn the_device_flow_is_grok_builds() {
    let flow = device_flow("1.0.77");
    assert_eq!(flow.client_id, CLIENT_ID);
    assert!(flow.scope.contains("grok-cli:access") && flow.scope.contains("offline_access"));
    assert_eq!(
        flow.headers,
        [("x-grok-client-version", "1.0.77".to_owned())]
    );
}
