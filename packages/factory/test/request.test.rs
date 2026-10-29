use super::*;
use crate::fixture;
#[test]
fn all_four_routes_keep_their_native_format() {
    for(path,model,wire)in[("/api/llm/a/v1/messages","claude-opus-5-5",Wire::Anthropic),("/api/llm/o/v1/responses","gpt-5.6-sol",Wire::Responses),("/api/llm/o/v1/chat/completions","glm-5.1",Wire::Chat),("/api/llm/g/v1/models/gemini-4-argon:streamGenerateContent?alt=sse&key=private-placeholder","gemini-4-argon",Wire::Gemini)]{let model=if models::find(model).is_some(){model.to_owned()}else{models::registry().into_iter().find(|m|models::wire(m)==Some(wire)).unwrap()["id"].as_str().unwrap().to_owned()};let path=if wire==Wire::Gemini{format!("/api/llm/g/v1/models/{model}:streamGenerateContent?alt=sse&key=private-placeholder")}else{path.into()};let parsed=parse(&fixture::fetch(&path,json!({"model":model,"messages":[{"role":"user","content":"question"}]}))).unwrap();assert_eq!(parsed.wire,wire);if wire==Wire::Gemini{assert_eq!(parsed.path,"/api/llm/g/v1/generate");assert!(parsed.query.is_empty());}assert_eq!(parsed.body["model"],model);}
}
#[test]
fn fast_mode_uses_the_real_native_model_id_and_beta() {
    let row = models::registry()
        .into_iter()
        .find(|m| {
            models::wire(m) == Some(Wire::Anthropic) && m["id"].as_str().unwrap().ends_with("-fast")
        })
        .unwrap();
    let fast = row["id"].as_str().unwrap();
    let base = fast.strip_suffix("-fast").unwrap();
    let parsed = parse(&fixture::fetch(
        "/api/llm/a/v1/messages",
        json!({"model":base,"speed":"fast","messages":[]}),
    ))
    .unwrap();
    assert_eq!(parsed.body["model"], fast);
    assert!(parsed.fast);
}
#[test]
fn foreign_origins_paths_protocols_and_model_ids_are_not_forwarded() {
    for (path, model) in [
        ("/api/llm/o/v1/responses", "claude-opus-5-5"),
        ("/api/llm/a/v1/messages", "unrecognized"),
        ("/api/cli/whoami", "claude-opus-5-5"),
        (
            "/api/llm/a/v1/messages?to=https://evil.test",
            "claude-opus-5-5",
        ),
    ] {
        assert!(parse(&fixture::fetch(path, json!({"model":model}))).is_err());
    }
    let mut r = fixture::fetch("/api/llm/a/v1/messages", json!({"model":"claude-opus-5-5"}));
    r.url = "https://api.factory.ai.evil.test/api/llm/a/v1/messages".into();
    assert!(parse(&r).is_err());
}
#[test]
fn unsupported_sampling_is_explicit_and_large_integer_tool_data_stays_exact() {
    assert!(parse(&fixture::fetch(
        "/api/llm/o/v1/responses",
        json!({"model":"gpt-5.6-sol","temperature":0.4})
    ))
    .is_err());
    let parsed=parse(&fixture::fetch("/api/llm/a/v1/messages",json!({"model":"claude-opus-5-5","messages":[{"role":"assistant","content":[{"type":"tool_use","name":"x","input":{"large":18446744073709551615u64}}]}]}))).unwrap();
    assert_eq!(
        parsed.body["messages"][0]["content"][0]["input"]["large"],
        18446744073709551615u64
    );
}
