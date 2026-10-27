use super::*;
use serde_json::json;

fn definition() -> Definition {
    Definition::parse(json!({"id":"compatible","name":"Custom","api":"openai-completions","baseURL":"https://custom.invalid/v1","config":{"keyProvider":"custom","extraBody":{"temperature":0.5,"!service_tier":"flex","model":"wrong","!stream":false,"metadata":{"who":"agent-${sessionId}"}}}})).unwrap()
}

#[test]
fn extra_body_fills_missing_fields_forces_only_explicit_fields_and_substitutes_nested_session_values(
) {
    let mut body = Some(
        br#"{"model":"actual","stream":true,"temperature":0.8,"service_tier":"auto"}"#.to_vec(),
    );
    apply(
        &definition(),
        &json!({"type":"apiKey"}),
        "/v1/responses",
        "conversation-one",
        &mut body,
    );
    let parsed: Value = serde_json::from_slice(body.as_ref().unwrap()).unwrap();
    assert_eq!(
        parsed,
        json!({"model":"actual","stream":true,"temperature":0.8,"service_tier":"flex","metadata":{"who":"agent-conversation-one"}})
    );
}

#[test]
fn extra_body_does_not_change_media_token_counting_or_subscriptions_with_their_own_requests() {
    for (path, credentials) in [
        ("/v1/images/generations", json!({"type":"apiKey"})),
        ("/v1/messages/count_tokens", json!({"type":"apiKey"})),
        ("/v1/responses", json!({"type":"oauth"})),
    ] {
        let mut body = Some(br#"{"model":"actual"}"#.to_vec());
        let original = body.clone();
        apply(&definition(), &credentials, path, "conversation", &mut body);
        assert_eq!(body, original);
    }
}

#[test]
fn codex_keeps_backend_required_fields_but_accepts_optional_extra_fields() {
    let mut definition = definition();
    definition.config.oauth_provider = Some("openai-codex".into());
    definition.config.extra.insert(
        "extraBody".into(),
        json!({"!store":true,"!tool_choice":"none","!include":[],"access_programs":["example"]}),
    );
    let mut body = Some(
        br#"{"store":false,"tool_choice":"auto","include":["reasoning.encrypted_content"]}"#
            .to_vec(),
    );
    apply(
        &definition,
        &json!({"type":"oauth"}),
        "/responses",
        "conversation",
        &mut body,
    );
    let parsed: Value = serde_json::from_slice(body.as_ref().unwrap()).unwrap();
    assert_eq!(parsed["store"], false);
    assert_eq!(parsed["tool_choice"], "auto");
    assert_eq!(parsed["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(parsed["access_programs"], json!(["example"]));
}

#[test]
fn session_keys_preserve_sorted_objects_and_array_order() {
    let request: FetchRequest = serde_json::from_value(serde_json::json!({"url":"https://example.invalid","method":"POST"})).unwrap();
    let a = br#"{"messages":[{"role":"user","content":[{"type":"text","text":"x"},{"type":"text","text":"y"}]}]}"#;
    let b = br#"{"messages":[{"content":[{"text":"x","type":"text"},{"text":"y","type":"text"}],"role":"user"}]}"#;
    let c = br#"{"messages":[{"content":[{"text":"y","type":"text"},{"text":"x","type":"text"}],"role":"user"}]}"#;
    assert_eq!(session(&request,Some(a)),session(&request,Some(b)));
    assert_ne!(session(&request,Some(b)),session(&request,Some(c)));
    assert_eq!(session(&request,Some(a)),"nora-ea022673a62cd704cad8c911802f1654");
}
