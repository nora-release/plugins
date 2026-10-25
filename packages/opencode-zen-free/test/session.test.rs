use super::*;
use std::collections::BTreeMap;
fn request(session: Option<&str>) -> FetchRequest {
    FetchRequest {
        subscription_accounts_enabled: true,
        account: "public".into(),
        credentials: None,
        network_proxy: None,
        url: String::new(),
        method: "POST".into(),
        headers: BTreeMap::new(),
        metadata: session
            .map(|session| BTreeMap::from([("sessionID".into(), session.into())]))
            .unwrap_or_default(),
        body: None,
        body_file: None,
    }
}
#[test]
fn session_ids_have_the_actual_opencode_counter_shape_and_random_tail() {
    let sessions = Sessions::default();
    for _ in 0..100 {
        let id = sessions.id("ses", true);
        assert!(real(&id));
        assert!(!real(&id.replace("ses_", "msg_")));
    }
    assert!(!real("ses_000000000000abcdefghijklmn"));
}
#[test]
fn host_sessions_are_stable_but_never_sent_as_raw_strings() {
    let sessions = Sessions::default();
    let one = sessions.for_request(&request(Some("private-one")), &serde_json::json!({}));
    assert_eq!(
        one,
        sessions.for_request(&request(Some("private-one")), &serde_json::json!({}))
    );
    assert_ne!(
        one,
        sessions.for_request(&request(Some("private-two")), &serde_json::json!({}))
    );
    assert!(real(&one));
    assert!(!one.contains("private"));
    let given = sessions.id("ses", true);
    assert_eq!(
        sessions.for_request(&request(Some(&given)), &serde_json::json!({})),
        given
    );
}
#[test]
fn session_cache_is_bounded_and_can_use_a_first_user_message_when_ids_are_absent() {
    let sessions = Sessions::default();
    let body = serde_json::json!({"messages":[{"role":"system","content":"system"},{"role":"user","content":"first"}]});
    assert_eq!(
        sessions.for_request(&request(None), &body),
        sessions.for_request(&request(None), &body)
    );
    for i in 0..1010 {
        sessions.for_request(&request(Some(&format!("session-{i}"))), &Value::Null);
    }
    assert_eq!(sessions.sessions.lock().unwrap().0.len(), 1000);
}
