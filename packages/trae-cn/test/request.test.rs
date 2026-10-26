use super::*;
use crate::tests::{answer, collect, configured, credentials, listing, Client};
use base64::{engine::general_purpose::STANDARD, Engine};
fn request(credentials: Value, body: Value, url: &str, session: Option<&str>) -> FetchRequest {
    serde_json::from_value(json!({"account":"member","credentials":credentials,"method":"POST","url":url,"headers":{"authorization":"Bearer other-account","x-magpie-trae-session":"untrusted-header"},"metadata":session.map(|session|BTreeMap::from([("sessionID",session)])).unwrap_or_default(),"body":STANDARD.encode(body.to_string())})).unwrap()
}
fn entry() -> models::Entry {
    models::Entry {
        id: "model-max".into(),
        name: "Model Max".into(),
        function: "solo_agent".into(),
        model_name: "model__max".into(),
        most: 64000,
        context: 1000000,
        sees: true,
        max: Some(("model".into(), 1000000)),
        levels: Some(Vec::new()),
    }
}
#[test]
fn native_history_keeps_tool_results_consecutive_and_puts_images_after_the_whole_run() {
    let chat = json!({"tools":[{"type":"function","function":{"name":"look"}}],"messages":[{"role":"assistant","content":"let me inspect"},{"role":"assistant","tool_calls":[{"id":"one","function":{"name":"look","arguments":"{}"}},{"id":"two","function":{"name":"look","arguments":"{}"}}]},{"role":"tool","tool_call_id":"one","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,aA=="}}]},{"role":"tool","tool_call_id":"two","content":""},{"role":"user","content":"continue"}]});
    let messages = messages(&chat, true).unwrap();
    assert_eq!(messages[0]["role"], "assistant");
    assert_eq!(messages[0]["tool_calls"].as_array().unwrap().len(), 2);
    assert_eq!(messages[1]["role"], "tool");
    assert_eq!(messages[2]["role"], "tool");
    assert_eq!(messages[2]["content"][0]["text"], "(no output)");
    assert_eq!(messages[3]["role"], "user");
    assert!(messages[3]["content"]
        .to_string()
        .contains("data:image/png"));
    assert!(messages[0]["content"]
        .to_string()
        .contains("let me inspect"));
}
#[test]
fn max_mode_names_its_real_model_and_reserves_reply_space_and_native_tools() {
    let chat = json!({"model":"model-max","messages":[{"role":"user","content":"hi"}],"max_tokens":128000,"tools":[{"type":"function","function":{"name":"read","parameters":{"type":"object","properties":{"path":{"type":"string"}}}}}],"tool_choice":{"type":"function","function":{"name":"read"}},"parallel_tool_calls":false});
    let actual = body(&chat, "solo_agent", &entry(), "session").unwrap();
    assert_eq!(actual["config_name"], "model");
    assert_eq!(actual["model_name"], "model__max");
    assert_eq!(actual["max_tokens"], 64000);
    assert_eq!(
        actual["user_message_context"]["model_info"]["prompt_max_tokens"],
        936000
    );
    assert!(actual["tools"][0]["function"]["parameters"].is_string());
    assert_eq!(actual["tool_choice"], "read");
    assert_eq!(actual["parallel_tool_calls"], false);
}
#[test]
fn conversation_identity_stays_per_session_and_per_account() {
    let provider = configured(Site::China, Client::new(vec![]));
    let credentials = credentials();
    let chat = json!({"messages":[{"role":"user","content":"first question"}]});
    let a = conversation(&provider, &chat, "host-session", &credentials);
    let mut more = chat.clone();
    more["messages"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"assistant","content":"answer"}));
    assert_eq!(
        a,
        conversation(&provider, &more, "host-session", &credentials)
    );
    let mut other = credentials.clone();
    other["uid"] = json!("other");
    assert_ne!(a, conversation(&provider, &chat, "host-session", &other));
    assert_eq!(
        conversation(&provider, &chat, "", &credentials),
        conversation(&provider, &more, "", &credentials)
    );
    assert!(uuid::Uuid::parse_str(&a).is_ok());
}
#[tokio::test]
async fn wrong_function_retries_keep_one_conversation_and_use_real_account_headers() {
    let client = Client::new(vec![
        (200, listing()),
        (400, json!({"code":4001,"message":"wrong function"})),
        (200, answer()),
    ]);
    let provider = configured(Site::China, client.clone());
    let result = fetch(
        &provider,
        request(
            credentials(),
            json!({"model":"model","messages":[{"role":"user","content":"hi"}],"stream":false}),
            "https://trae-api-cn.mchost.guru/v1/chat/completions",
            Some("host-session"),
        ),
    )
    .await
    .unwrap();
    assert_eq!(result.status, 200);
    let value: Value = serde_json::from_slice(&collect(result).await).unwrap();
    assert_eq!(value["choices"][0]["message"]["content"], "hello");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[1].headers["authorization"],
        "Cloud-IDE-JWT jwt-one"
    );
    assert!(!requests[1].headers.contains_key("x-magpie-trae-session"));
    let a: Value = serde_json::from_slice(requests[1].body.as_ref().unwrap()).unwrap();
    let b: Value = serde_json::from_slice(requests[2].body.as_ref().unwrap()).unwrap();
    assert_eq!(a["session_id"], b["session_id"]);
    assert_ne!(a["request_id"], b["request_id"]);
    assert_eq!(a["function"], "chat_v3");
    assert_eq!(b["function"], "solo_work_lite");
}
#[tokio::test]
async fn foreign_chat_addresses_never_receive_the_accounts_jwt() {
    let client = Client::new(vec![]);
    let provider = configured(Site::Global, client.clone());
    assert!(fetch(
        &provider,
        request(
            credentials(),
            json!({"model":"model","messages":[]}),
            "https://attacker.example/v1/chat/completions",
            None
        )
    )
    .await
    .is_err());
    assert!(client.requests.lock().unwrap().is_empty());
}
