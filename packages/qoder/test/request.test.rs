use super::*;
use crate::tests::{answer, catalog, collect, configured, credentials, Client};
use base64::{engine::general_purpose::STANDARD, Engine};
fn fetch_request(credentials: Value, body: Value, url: String) -> FetchRequest {
    serde_json::from_value(json!({"account":"member","credentials":credentials,"method":"POST","url":url,"headers":{"authorization":"Bearer wrong-account"},"body":STANDARD.encode(body.to_string())})).unwrap()
}
#[test]
fn tool_results_with_lost_ids_reserve_the_calls_later_results_still_name() {
    let out=messages(&[json!({"role":"assistant","tool_calls":[{"id":"first","function":{"name":"one","arguments":"{}"}},{"id":"second","function":{"name":"two","arguments":{}}}]}),json!({"role":"tool","content":"lost result"}),json!({"role":"tool","tool_call_id":"first","content":"named result"})]).unwrap();
    assert_eq!(out[1]["tool_call_id"], "second");
    assert_eq!(out[2]["tool_call_id"], "first");
}
#[test]
fn orphan_tool_text_and_images_remain_in_the_conversation_and_generated_call_ids_pair() {
    let out=messages(&[json!({"role":"assistant","tool_calls":[{"function":{"name":"look","arguments":{}}}]}),json!({"role":"tool","content":[{"type":"text","text":"description"},{"type":"image_url","image_url":{"url":"data:image/png;base64,aA=="}}]}),json!({"role":"tool","content":"orphan text"}),json!({"role":"user","content":"continue"})]).unwrap();
    assert_eq!(out[1]["tool_call_id"], out[0]["tool_calls"][0]["id"]);
    assert!(out[1]["content"].as_str().unwrap().contains("image"));
    let user = out.last().unwrap()["content"].to_string();
    assert!(user.contains("orphan text"));
    assert!(user.contains("data:image/png"));
    assert!(user.contains("continue"));
}
#[test]
fn context_window_includes_reply_and_keeps_the_cheaper_default_while_it_fits() {
    let entries=models::listing(&json!({"app":[{"key":"model","enable":true,"context_config":{"small":{"token_count":100,"is_default":true},"big":{"token_count":1000}}}]})).unwrap();
    let model = &entries[0];
    let chat = json!({"model":"model","messages":[{"role":"system","content":"preserve these instructions"},{"role":"user","content":"hi"}],"max_tokens":150,"tool_choice":"required","tools":[{"type":"function","function":{"name":"read","parameters":{"type":"object"}}}]});
    let value = body(&chat, model).unwrap();
    assert_eq!(value["parameters"]["context_length"], 1000);
    assert!(value["system"][0]["text"]
        .as_str()
        .unwrap()
        .contains("preserve these instructions"));
    assert!(value["system"][0]["text"]
        .as_str()
        .unwrap()
        .contains("must call"));
    assert_eq!(window(&json!({"messages":[],"tools":[]}), model, 10), 100);
    assert_eq!(window(&json!({"messages":[]}), model, 150), 1000);
}
#[tokio::test]
async fn chat_uses_the_live_model_config_encoded_wire_and_real_cosy_endpoint() {
    let client = Client::new(vec![(200, catalog()), (200, answer())]);
    let provider = configured(Site::International, client.clone());
    let response = fetch(
        &provider,
        fetch_request(
            credentials(),
            json!({"model":"model","messages":[{"role":"user","content":"hi"}],"stream":false}),
            "https://api3.qoder.sh/chat/completions".into(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(response.status, 200);
    let value: Value = serde_json::from_slice(&collect(response).await).unwrap();
    assert_eq!(value["choices"][0]["message"]["content"], "hello");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[1]
        .url
        .contains("/algo/api/v2/service/pro/sse/agent_chat_generation?FetchKeys="));
    assert_eq!(requests[1].headers["x-model-key"], "model");
    assert!(requests[1].headers["authorization"].starts_with("Bearer COSY."));
    let plaintext =
        codec::decode(std::str::from_utf8(requests[1].body.as_ref().unwrap()).unwrap()).unwrap();
    let actual: Value = serde_json::from_str(&plaintext).unwrap();
    assert_eq!(actual["model_config"]["key"], "model");
    assert_eq!(actual["parameters"]["max_tokens"], 32000);
}
#[tokio::test]
async fn custom_host_is_rejected_before_credentials_or_model_requests_leave() {
    let client = Client::new(vec![]);
    let provider = configured(Site::International, client.clone());
    assert!(fetch(
        &provider,
        fetch_request(
            credentials(),
            json!({"model":"model","messages":[]}),
            "https://attacker.example/chat/completions".into()
        )
    )
    .await
    .is_err());
    assert!(client.requests.lock().unwrap().is_empty());
}
