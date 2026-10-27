use super::*;
use serde_json::json;

fn finalized(oauth: bool, target: &str, original: &Value) -> Value {
    let mut body = Some(serde_json::to_vec(original).unwrap());
    responses_body::finalize(oauth, &Url::parse(target).unwrap(), &mut body);
    serde_json::from_slice(&body.unwrap()).unwrap()
}

#[test]
fn codex_streams_without_storage_and_preserves_native_options() {
    for store in [Value::Null, json!(false), json!(true)] {
        let original = json!({"model":"m","input":"hi","store":store,"stream":false,"background":true,"include":["reasoning.encrypted_content"],"client_metadata":{"window":"w"}});
        let body = finalized(
            true,
            "https://chatgpt.com/backend-api/codex/responses",
            &original,
        );
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        for key in ["background", "include", "client_metadata", "input"] {
            assert_eq!(body[key], original[key]);
        }
    }
}

#[test]
fn ordinary_api_and_legacy_compact_keep_the_callers_values() {
    for (oauth, target) in [
        (false, "https://api.openai.com/v1/responses"),
        (
            true,
            "https://chatgpt.com/backend-api/codex/responses/compact",
        ),
        (true, "https://relay.example/backend-api/codex/responses"),
    ] {
        let original = json!({"model":"m","input":"hi","store":true,"stream":false,"background":true,"include":[],"client_metadata":null});
        assert_eq!(finalized(oauth, target, &original), original);
    }
}

#[test]
fn codex_encrypted_reasoning_include_matches_each_effort_and_existing_list() {
    for effort in [None, Some("none"), Some("high")] {
        for include in [
            None,
            Some(json!([])),
            Some(json!([
                "message.output_text.logprobs",
                "message.output_text.logprobs",
                ""
            ])),
            Some(json!([
                "reasoning.encrypted_content",
                "reasoning.encrypted_content"
            ])),
        ] {
            let mut original = json!({"model":"m","input":"hi"});
            if let Some(effort) = effort {
                original["reasoning"] = json!({"effort":effort});
            }
            if let Some(include) = &include {
                original["include"] = include.clone();
            }
            let actual = finalized(
                true,
                "https://chatgpt.com/backend-api/codex/responses",
                &original,
            );
            if effort == Some("none") {
                assert!(actual.get("include").is_none());
                continue;
            }
            let mut expected = include
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default();
            if !expected.contains(&json!("reasoning.encrypted_content")) {
                expected.push(json!("reasoning.encrypted_content"));
            }
            assert_eq!(actual["include"], Value::Array(expected), "{effort:?}");
        }
    }
}

#[test]
fn codex_final_body_combines_fast_mode_and_native_request_rules() {
    for tier in ["auto", "flex", "priority", "ultrafast"] {
        let original = json!({"model":"m","input":"hi","service_tier":tier,"stream":false,"store":true,"background":true});
        let actual = finalized(
            true,
            "https://chatgpt.com/backend-api/codex/responses",
            &original,
        );
        assert_eq!(
            actual.get("service_tier").and_then(Value::as_str),
            matches!(tier, "priority" | "ultrafast").then_some(tier)
        );
        assert_eq!(actual["store"], false);
        assert_eq!(actual["stream"], true);
        assert_eq!(actual["background"], true);
        assert_eq!(actual["include"], json!(["reasoning.encrypted_content"]));
    }
}
