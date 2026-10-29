use super::*;
use crate::tests::{configured, data, Client};
use base64::{engine::general_purpose::STANDARD, Engine};
use futures::StreamExt;

fn request(body: Value) -> FetchRequest {
    FetchRequest {
        caller_user_agent: None,
        constraints: Default::default(),
        subscription_accounts_enabled: true,
        account: "member".into(),
        credentials: Some(
            json!({"type":"oauth","access":"access","uid":"member","domain":"tenant.example","enterpriseId":"team"}),
        ),
        network_proxy: None,
        url: "https://copilot.tencent.com/v2/chat/completions".into(),
        method: "POST".into(),
        headers: BTreeMap::from([("authorization".into(), "Bearer untrusted".into())]),
        metadata: BTreeMap::from([
            ("sessionID".into(), "conversation".into()),
            ("turnID".into(), "turn".into()),
        ]),
        body: Some(STANDARD.encode(body.to_string())),
        body_file: None,
    }
}

#[test]
fn request_shapes_keep_the_user_ask_after_tools_and_move_refused_systems_without_losing_content() {
    let mut body = json!({"model":"m","max_completion_tokens":123,"messages":[{"role":"developer","content":"instructions"},{"role":"tool","tool_call_id":"call","content":"done"},{"role":"user","content":"finish the task"}]});
    prepare(&mut body).unwrap();
    assert_eq!(body["max_tokens"], 123);
    assert_eq!(body["messages"][0]["role"], "system");
    assert!(after_tools(&mut body));
    assert_eq!(body["messages"][2]["role"], "assistant");
    assert_eq!(body["messages"][3]["content"], "finish the task");
    assert!(system_as_user(&mut body));
    assert_eq!(
        body["messages"][0]["content"],
        "You are a helpful assistant."
    );
    assert!(body["messages"].to_string().contains("<instructions>"));
    assert!(body["messages"].to_string().contains("finish the task"));
}

#[test]
fn conversation_ids_are_stable_within_a_turn_and_change_for_the_next_user_send() {
    let mut first = BTreeMap::new();
    let mut second = BTreeMap::new();
    let mut next = BTreeMap::new();
    let body = json!({"messages":[{"role":"user","content":"hello"}]});
    let metadata = BTreeMap::from([
        ("sessionID".into(), "session".into()),
        ("turnID".into(), "one".into()),
    ]);
    attend(&mut first, &body, &metadata, "salt");
    attend(&mut second, &body, &metadata, "salt");
    let mut changed = metadata;
    changed.insert("turnID".into(), "two".into());
    attend(&mut next, &body, &changed, "salt");
    assert_eq!(first["x-conversation-id"], next["x-conversation-id"]);
    assert_eq!(
        first["x-conversation-request-id"],
        second["x-conversation-request-id"]
    );
    assert_ne!(
        first["x-conversation-message-id"],
        second["x-conversation-message-id"]
    );
    assert_ne!(
        first["x-conversation-request-id"],
        next["x-conversation-request-id"]
    );
}

#[tokio::test]
async fn vendor_refusal_retries_the_same_request_with_its_system_as_user_and_preserves_team_headers(
) {
    let client = Client::new(vec![
        (
            400,
            json!({"code":11128,"msg":"Illegal API invocation from an unapproved channel"}),
        ),
        (200, json!({"raw":"data: [DONE]\n\n"})),
    ]);
    let provider = configured(Site::China, client.clone());
    let mut response=fetch(&provider,request(json!({"model":"m","messages":[{"role":"system","content":"Custom agent instructions"},{"role":"user","content":"hello"}]}))).await.unwrap();
    assert_eq!(response.status, 200);
    while response.body.next().await.is_some() {}
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].headers["x-enterprise-id"], "team");
    assert_eq!(requests[0].headers["authorization"], "Bearer access");
    assert_eq!(requests[0].headers["user-agent"], "WorkBuddy/5.5.6");
    assert_eq!(
        requests[0].headers["x-conversation-request-id"],
        requests[1].headers["x-conversation-request-id"]
    );
    let body: Value = serde_json::from_slice(requests[1].body.as_ref().unwrap()).unwrap();
    assert_eq!(
        body["messages"][0]["content"],
        "You are a helpful assistant."
    );
    assert!(body["messages"][1]["content"]
        .as_str()
        .unwrap()
        .contains("Custom agent instructions"));
}

#[tokio::test]
async fn dedicated_endpoint_validation_prevents_forwarding_a_workbuddy_token_elsewhere() {
    let client = Client::new(vec![data(json!({}))]);
    let provider = configured(Site::China, client.clone());
    let mut request = request(json!({"messages":[{"role":"user","content":"hi"}]}));
    request.url = "https://attacker.example/chat/completions".into();
    assert!(fetch(&provider, request).await.is_err());
    assert!(client.requests.lock().unwrap().is_empty());
}

fn after_tool_body(model: &str) -> Value {
    json!({"model":model,"messages":[{"role":"system","content":"Specific task instructions"},{"role":"user","content":"List the files."},{"role":"assistant","content":null,"tool_calls":[{"id":"call-one","type":"function","function":{"name":"read","arguments":"{}"}}]},{"role":"tool","tool_call_id":"call-one","content":"a.txt\nb.txt"},{"role":"user","content":[{"type":"text","text":"Continue with these results."}]}]})
}

fn after_tool_refusal() -> (u16, Value) {
    (
        400,
        json!({"code":11153,"msg":"the conversation must end with a user message"}),
    )
}

#[tokio::test]
async fn successive_system_and_after_tool_refusals_are_repaired_once_and_reused_on_the_next_request(
) {
    let client = Client::new(vec![
        (
            400,
            json!({"code":11128,"msg":"Illegal API invocation from an unapproved channel"}),
        ),
        after_tool_refusal(),
        (200, json!({"raw":"data: [DONE]\n\n"})),
        (200, json!({"raw":"data: [DONE]\n\n"})),
    ]);
    let provider = configured(Site::China, client.clone());
    let body = after_tool_body("model-one");
    for _ in 0..2 {
        assert_eq!(
            fetch(&provider, request(body.clone()))
                .await
                .unwrap()
                .status,
            200
        );
    }
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    let sent: Vec<Value> = requests
        .iter()
        .map(|request| serde_json::from_slice(request.body.as_ref().unwrap()).unwrap())
        .collect();
    assert_eq!(sent[2], sent[3]);
    assert_eq!(
        sent[2]["messages"][0]["content"],
        "You are a helpful assistant."
    );
    let messages = sent[2]["messages"].as_array().unwrap();
    assert_eq!(
        messages[messages.len() - 2],
        json!({"role":"assistant","content":""})
    );
    assert_eq!(
        messages.last().unwrap(),
        body["messages"].as_array().unwrap().last().unwrap()
    );
    assert_eq!(
        messages
            .iter()
            .filter(|message| message["role"] == "tool")
            .count(),
        1
    );
    for request in requests.iter() {
        assert_eq!(request.headers["x-enterprise-id"], "team");
        assert_eq!(request.headers["authorization"], "Bearer access");
    }
}

#[tokio::test]
async fn after_tool_repair_is_learned_for_the_refusing_model_without_changing_another_model() {
    let client = Client::new(vec![
        after_tool_refusal(),
        (200, json!({"raw":"data: [DONE]\n\n"})),
        (200, json!({"raw":"data: [DONE]\n\n"})),
        (200, json!({"raw":"data: [DONE]\n\n"})),
    ]);
    let provider = configured(Site::China, client.clone());
    for model in ["needs-repair", "needs-repair", "another-model"] {
        assert_eq!(
            fetch(&provider, request(after_tool_body(model)))
                .await
                .unwrap()
                .status,
            200
        );
    }
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    for (index, expected) in [(0, 0), (1, 1), (2, 1), (3, 0)] {
        let value: Value = serde_json::from_slice(requests[index].body.as_ref().unwrap()).unwrap();
        assert_eq!(
            value["messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|message| message["role"] == "assistant" && message["content"] == "")
                .count(),
            expected
        );
    }
}

#[tokio::test]
async fn an_after_tool_repair_refused_again_returns_the_vendor_error_without_an_unbounded_retry() {
    let client = Client::new(vec![after_tool_refusal(), after_tool_refusal()]);
    let provider = configured(Site::China, client.clone());
    let response = fetch(&provider, request(after_tool_body("refuses-again")))
        .await
        .unwrap();
    assert_eq!(response.status, 400);
    assert_eq!(client.requests.lock().unwrap().len(), 2);
    let bytes = response
        .body
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<PluginResult<Vec<_>>>()
        .unwrap()
        .concat();
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap()["code"],
        11153
    );
}

#[tokio::test]
async fn an_assistant_prefill_is_preserved_and_is_never_retried_as_a_fake_user_turn() {
    let client = Client::new(vec![after_tool_refusal()]);
    let provider = configured(Site::China, client.clone());
    let mut body = after_tool_body("prefill");
    body["messages"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"assistant","content":"There are"}));
    let response = fetch(&provider, request(body)).await.unwrap();
    assert_eq!(response.status, 400);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let value: Value = serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(
        value["messages"].as_array().unwrap().last().unwrap(),
        &json!({"role":"assistant","content":"There are"})
    );
}
