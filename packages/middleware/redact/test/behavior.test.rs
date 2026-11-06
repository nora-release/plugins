use nora_provider_redact::{Engine, Settings, StreamState};
use serde_json::{json, Value};
#[test]
fn streaming_restores_every_byte_split_and_keeps_comments_live() {
    let original = "sk-proj-abcdefghijklmnopqrstuvwxyz123456789";
    for protocol in 0..3 {
        let mut e = Engine::new([7; 32]);
        let p = e.mask(
            original,
            &Settings {
                redact: true,
                ..Default::default()
            },
            false,
        );
        for cut in 1..p.len() {
            let frame = |text: &str| {
                format!(
                    "data: {}\n\n",
                    match protocol {
                        0 => json!({"choices":[{"index":0,"delta":{"content":text}}]}),
                        1 =>
                            json!({"type":"response.output_text.delta","item_id":"item","output_index":0,"content_index":0,"delta":text}),
                        _ =>
                            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}),
                    }
                )
            };
            let a = frame(&p[..cut]);
            let b = frame(&p[cut..]);
            let mut state = StreamState::new("text/event-stream", "");
            let mut result = Vec::new();
            for byte in a.bytes() {
                result.extend(state.push(&e, &[byte], false).unwrap());
            }
            let comment = state.push(&e, b": keepalive\n", false).unwrap();
            assert!(String::from_utf8_lossy(&comment).contains(": keepalive"));
            result.extend(comment);
            for byte in b.bytes() {
                result.extend(state.push(&e, &[byte], false).unwrap());
            }
            result.extend(state.push(&e, b"data: [DONE]\n\n", true).unwrap());
            let output = String::from_utf8(result).unwrap();
            let mut text = String::new();
            for line in output.lines() {
                if let Some(s) = line.strip_prefix("data: ") {
                    if let Ok(v) = serde_json::from_str::<Value>(s) {
                        text.push_str(match protocol {
                            0 => v["choices"][0]["delta"]["content"].as_str().unwrap(),
                            1 => v["delta"].as_str().unwrap(),
                            _ => v["delta"]["text"].as_str().unwrap(),
                        });
                    }
                }
            }
            assert_eq!(text, original, "protocol {protocol}, cut {cut}");
        }
    }
}
#[test]
fn words_rules_actions_preview_and_known_history_use_one_engine() {
    let mut e = Engine::new([7; 32]);
    let settings:Settings=serde_json::from_value(json!({"redact":true,"redactPersonal":true,"redactWords":["夜莺","Nightjar"],"redactWordsAnyCase":true,"redactRules":[{"kind":"Private relay","prefix":"relay_"}],"redactActions":{"EMAIL":"warn"},"redactAllow":["sk-proj-allow123456789012345678901234"]})).unwrap();
    let settings = settings.validate().unwrap();
    let original =
        "夜莺 NIGHTJAR relay_a1b2c3d4 bob@quill.dev sk-proj-allow123456789012345678901234";
    let out = e.mask(original, &settings, false);
    assert!(!out.contains("NIGHTJAR"));
    assert!(!out.contains("relay_a1"));
    assert!(out.contains("bob@quill.dev"));
    assert!(out.contains("sk-proj-allow"));
    assert_eq!(e.restore(&out, false), original);
    assert!(e
        .hits
        .iter()
        .any(|h| h.action == "warn" && h.kind == "EMAIL"));
    let s = Settings {
        redact: true,
        ..Default::default()
    };
    let historic = "the old value Tr0ub4dor&3xyz";
    assert_eq!(e.mask(historic, &s, false), historic);
    let key = e.mask("DB_PASSWORD=Tr0ub4dor&3xyz", &s, false);
    assert!(key.contains("{{SECRET_"));
    assert_eq!(e.mask(historic, &s, false), historic);
    assert!(e
        .mask("a new reference to Tr0ub4dor&3xyz", &s, false)
        .contains("{{SECRET_"));
    let p = e.preview("API_KEY=Abcd1234xyz", &s);
    assert_eq!(e.restore(&p, false), p);
}
#[test]
fn restore_keeps_argument_json_escaping_and_identifiers() {
    let mut e = Engine::new([7; 32]);
    let s = Settings {
        redact: true,
        ..Default::default()
    };
    let key = "-----BEGIN PRIVATE KEY-----\nprivate\n-----END PRIVATE KEY-----";
    let p = e.mask(key, &s, false);
    let body=json!({"type":"response.function_call_arguments.delta","delta":format!("{{\"key\":\"{p}\"}}")}).to_string();
    let restored: Value = serde_json::from_str(&e.restore_json(&body)).unwrap();
    let args: Value = serde_json::from_str(restored["delta"].as_str().unwrap()).unwrap();
    assert_eq!(args["key"], key);
}

#[test]
fn official_settings_normalize_before_limits_and_drop_empty_rules() {
    let settings: nora_provider_redact::Settings = serde_json::from_value(serde_json::json!({
        "redactRules": (0..40).map(|_| serde_json::json!({"kind":"token","prefix":"  acme_  "})).chain(std::iter::once(serde_json::json!({"kind":"empty"}))).collect::<Vec<_>>(),
        "redactAllow": (0..240).map(|_| "  EXAMPLE.TEST  ").collect::<Vec<_>>(),
        "redactActions": {" TOKEN ":" WARN ", "PASSWORD":" Mask ","API_KEY":" "}
    })).unwrap();
    let normalized = settings.validate().unwrap();
    assert_eq!(normalized.redact_rules.len(), 1);
    assert_eq!(normalized.redact_allow, vec!["EXAMPLE.TEST"]);
    assert_eq!(
        normalized.redact_actions,
        std::collections::BTreeMap::from([("TOKEN".into(), "warn".into())])
    );
}

#[test]
fn sealed_history_preserves_prior_bytes_and_only_matches_standalone_known_secrets() {
    let mut engine = Engine::new([7; 32]);
    let settings = Settings {
        redact: true,
        redact_personal: true,
        ..Default::default()
    };
    let secret = "Wv7otterPass93";
    let sealed = |s: &str| {
        json!({"messages":[{"role":"assistant","content":[{"type":"thinking","thinking":s,"signature":"Eqk1"}]}]}).to_string()
    };
    let old = sealed(&format!("the pass is {secret}"));
    assert_eq!(engine.mask(&old, &settings, true), old);
    engine.mask(&format!("DB_PASSWORD={secret}"), &settings, false);
    assert_eq!(engine.mask(&old, &settings, true), old);
    let inside = sealed(&format!("pass{secret}x"));
    assert_eq!(engine.mask(&inside, &settings, true), inside);
    let new = sealed(&format!("another pass is {secret}"));
    assert!(!engine.mask(&new, &settings, true).contains(secret));
}

#[test]
fn binary_media_responses_are_preserved_when_json_restoration_does_not_apply() {
    let engine = Engine::new([7; 32]);
    let bytes = b"\x89PNG\r\n\x1a\n\xff\x00binary";
    let mut stream = StreamState::new("image/png", "");
    assert!(stream.push(&engine, bytes, false).unwrap().is_empty());
    assert_eq!(stream.push(&engine, &[], true).unwrap(), bytes);
}
