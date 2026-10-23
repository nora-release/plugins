use super::*;
use crate::fixture;
#[test]
fn anthropic_envelope_preserves_system_tools_and_tool_result_ids_with_required_error_flag() {
    let request = fixture::fetch(
        "/v1/messages?beta=true",
        json!({"model":"claude-model","stream":true,"system":[{"type":"text","text":"system"}],"messages":[{"role":"assistant","content":[{"type":"tool_use","id":"original","name":"f","input":{}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"original","content":"ok"}]}],"tools":[{"name":"f","input_schema":{"type":"object"}}]}),
    );
    let parsed = parse(&request).unwrap();
    assert!(parsed.streaming);
    assert!(parsed.body.get("stream").is_none());
    assert_eq!(parsed.body["messages"][1]["content"][0]["is_error"], false);
    assert_eq!(
        parsed.body["messages"][1]["content"][0]["tool_use_id"],
        "original"
    );
    assert_eq!(parsed.envelope("anthropic")["provider"], "anthropic");
    assert_eq!(parsed.body["system"][0]["text"], "system");
}
#[test]
fn google_files_and_signatures_stay_in_the_original_gemini_envelope() {
    let request = fixture::fetch(
        "/v1/models/gemini-model:streamGenerateContent?alt=sse&key=must-not-forward-client-key",
        json!({"contents":[{"role":"user","parts":[{"inlineData":{"mimeType":"application/pdf","data":"cGRm"}},{"fileData":{"mimeType":"video/mp4","fileUri":"gs://explicit/file"}}]}],"sessionId":"local-only"}),
    );
    let parsed = parse(&request).unwrap();
    assert_eq!(parsed.wire, Wire::Gemini);
    assert_eq!(parsed.body["model"], "models/gemini-model");
    assert!(parsed.body.get("sessionId").is_none());
    assert!(!parsed
        .envelope("google")
        .to_string()
        .contains("must-not-forward-client-key"));
    assert_eq!(
        parsed.body["contents"][0]["parts"][0]["inlineData"]["data"],
        "cGRm"
    );
    assert_eq!(
        parsed.body["contents"][0]["parts"][1]["fileData"]["fileUri"],
        "gs://explicit/file"
    );
}
#[test]
fn responses_and_xai_keep_their_distinct_native_shapes() {
    let responses=parse(&fixture::fetch("/v1/responses",json!({"model":"gpt-model","stream":false,"input":[{"type":"function_call_output","call_id":"c1","output":"ok"}],"tools":[{"type":"function","name":"f","parameters":{"type":"object"}}]}))).unwrap();
    assert!(!responses.streaming);
    assert_eq!(responses.body["stream"], true);
    assert_eq!(responses.body["input"][0]["call_id"], "c1");
    let chat = parse(&fixture::fetch(
        "/v1/chat/completions",
        json!({"model":"grok-model","max_tokens":100,"messages":[]}),
    ))
    .unwrap();
    assert_eq!(chat.body["max_completion_tokens"], 100);
    assert!(chat.body.get("max_tokens").is_none());
}
#[test]
fn foreign_endpoints_and_unrecognized_query_parameters_are_rejected() {
    let mut request = fixture::fetch("/v1/messages", json!({"model":"claude-model"}));
    request.url = "https://another.example/v1/messages".into();
    assert!(parse(&request).is_err());
    for path in [
        "/v1/messages?api_key=private",
        "/v1/models/a%2Fb:generateContent",
        "/v1/models/x:other",
        "/other",
    ] {
        assert!(parse(&fixture::fetch(path, json!({"model":"x"}))).is_err());
    }
}
