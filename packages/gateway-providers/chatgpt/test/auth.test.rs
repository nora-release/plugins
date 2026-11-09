use super::*;
use base64::Engine;

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
fn a_token_answer_becomes_the_account_named_by_its_email() {
    let id = jwt(
        json!({"email": "ada@example.test", "https://api.openai.com/auth": {"chatgpt_account_id": "acct-1", "chatgpt_plan_type": "plus"}}),
    );
    let access = jwt(json!({"exp": 1_900_000_000u64}));
    let saved = credentials(
        &Value::Null,
        &json!({"access_token": access, "refresh_token": "r1", "id_token": id}),
    )
    .unwrap();
    assert_eq!(
        (
            text(&saved, "email"),
            text(&saved, "accountID"),
            text(&saved, "plan")
        ),
        ("ada@example.test", "acct-1", "plus")
    );
    let account = account(saved.clone());
    assert_eq!(account.name, "ada@example.test");
    assert_eq!(account.expires_at.as_deref(), Some("2030-03-17T17:46:40Z"));

    // a refresh that names no new id token keeps what was known
    let renewed = credentials(
        &saved,
        &json!({"access_token": jwt(json!({"exp": 1})), "refresh_token": "r2"}),
    )
    .unwrap();
    assert_eq!(
        (text(&renewed, "refresh"), text(&renewed, "email")),
        ("r2", "ada@example.test")
    );
    assert!(
        credentials(&Value::Null, &json!({"access_token": "a"})).is_err(),
        "an account without its id"
    );
    assert!(credentials(&saved, &json!({})).is_err());
}
