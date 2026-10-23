use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
fn account() -> Account {
    Account::saved(
        &json!({"key":"ABSK-key","region":"eu-west-1"}),
        &json!({}),
        false,
    )
    .unwrap()
}
fn fetch(path: &str, body: Value) -> FetchRequest {
    serde_json::from_value(json!({"url":format!("https://bedrock-runtime.eu-west-1.amazonaws.com{path}"),"method":"POST","headers":{"authorization":"Bearer untrusted","x-api-key":"untrusted","anthropic-beta":"oauth-2025-04-20,advanced-tool-use-2025-11-20,redact-thinking-2026-02-12,context-1m-2025-08-07"},"body":STANDARD.encode(body.to_string())})).unwrap()
}
#[test]
fn routes_claude_closed_gpt_and_open_models_on_their_native_protocols() {
    let account = account();
    for (id, api, path) in [
        (
            "global.anthropic.claude-opus-5-5",
            "anthropic-messages",
            "/anthropic",
        ),
        ("global.openai.gpt-6-luna", "openai-responses", "/openai/v1"),
        ("openai.gpt-oss-120b", "openai-completions", "/openai/v1"),
    ] {
        let got = route(id, &account, false);
        assert_eq!(got.0, api);
        assert!(got.1.ends_with(path));
    }
    assert_eq!(
        route("xai.grok-4.3", &account, true).0,
        "openai-completions"
    );
    assert!(route("google.gemma-3-27b-it", &account, true)
        .1
        .ends_with("/v1"));
}
#[test]
fn anthropic_uses_one_key_removes_only_user_id_and_fits_betas() {
    let request = fetch(
        "/anthropic/v1/messages",
        json!({"model":"global.anthropic.claude-opus-5-5","metadata":{"user_id":"{device_id:private}","other":"keep"},"anthropic_beta":["advanced-tool-use-2025-11-20","redact-thinking-2026-02-12","context-1m-2025-08-07"],"messages":[],"max_tokens":20}),
    );
    let upstream = prepare(&request, &account(), false, false, &BTreeSet::new()).unwrap();
    assert_eq!(upstream.headers.get("x-api-key").unwrap(), "ABSK-key");
    assert!(!upstream.headers.contains_key("authorization"));
    assert_eq!(
        upstream.headers["anthropic-beta"],
        "tool-search-tool-2025-10-19,context-1m-2025-08-07"
    );
    let body: Value = serde_json::from_slice(upstream.body.as_ref().unwrap()).unwrap();
    assert!(body["metadata"].get("user_id").is_none());
    assert_eq!(body["metadata"]["other"], "keep");
    assert_eq!(body["max_tokens"], 20);
    assert_eq!(
        body["anthropic_beta"],
        json!(["tool-search-tool-2025-10-19", "context-1m-2025-08-07"])
    );
}
#[test]
fn completions_use_completion_budget_and_do_not_rewrite_tool_results() {
    let request = fetch(
        "/openai/v1/chat/completions",
        json!({"model":"openai.gpt-oss-120b","max_tokens":24,"messages":[{"role":"tool","tool_call_id":"exact-id","content":"result"}]}),
    );
    let upstream = prepare(&request, &account(), false, false, &BTreeSet::new()).unwrap();
    assert_eq!(upstream.headers["authorization"], "Bearer ABSK-key");
    assert!(!upstream.headers.contains_key("x-api-key"));
    let body: Value = serde_json::from_slice(upstream.body.as_ref().unwrap()).unwrap();
    assert_eq!(body["max_completion_tokens"], 24);
    assert!(body.get("max_tokens").is_none());
    assert_eq!(body["messages"][0]["tool_call_id"], "exact-id");
}
#[test]
fn mantle_picks_and_can_flip_prefix_for_the_same_model() {
    let mut request = fetch(
        "/openai/v1/responses",
        json!({"model":"openai.gpt-5.5","input":"hi"}),
    );
    request.url = "https://bedrock-mantle.eu-west-1.api.aws/v1/responses".into();
    assert!(prepare(&request, &account(), true, false, &BTreeSet::new())
        .unwrap()
        .url
        .ends_with("/openai/v1/responses"));
    assert!(prepare(&request, &account(), true, true, &BTreeSet::new())
        .unwrap()
        .url
        .ends_with("/v1/responses"));
}
#[test]
fn requests_cannot_send_account_keys_to_another_region_host_or_operation() {
    for target in [
        "https://evil.example/openai/v1/responses",
        "https://bedrock-runtime.us-east-1.amazonaws.com/openai/v1/responses",
        "https://bedrock-runtime.eu-west-1.amazonaws.com/openai/v1/models",
        "https://bedrock-runtime.eu-west-1.amazonaws.com/openai/v1/responses?key=x",
    ] {
        let mut request = fetch("/openai/v1/responses", json!({"model":"openai.gpt-5.5"}));
        request.url = target.into();
        assert!(prepare(&request, &account(), false, false, &BTreeSet::new()).is_err());
    }
    assert!(prepare(
        &fetch("/openai/v1/responses", json!([])),
        &account(),
        false,
        false,
        &BTreeSet::new()
    )
    .is_err());
}
#[test]
fn aws_converse_signs_the_actual_body_and_keeps_session_credentials() {
    let account=Account::saved(&json!({"access":json!({"accessKeyId":"AKIA-own","secretAccessKey":"secret","sessionToken":"session"}).to_string(),"region":"eu-west-1"}),&json!({}),false).unwrap();
    let request = fetch(
        "/model/anthropic.claude-sonnet-v1/converse-stream",
        json!({"messages":[{"role":"user","content":[{"text":"hi"}]}]}),
    );
    let upstream = prepare(&request, &account, false, false, &BTreeSet::new()).unwrap();
    assert!(upstream
        .headers
        .iter()
        .any(|(name, value)| name.eq_ignore_ascii_case("authorization")
            && value.contains("AWS4-HMAC-SHA256")
            && value.contains("/eu-west-1/bedrock/aws4_request")));
    assert!(upstream.headers.iter().any(|(name, value)| name
        .eq_ignore_ascii_case("x-amz-security-token")
        && value == "session"));
}
