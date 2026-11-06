use nora_gateway_middleware::{Action, Context, Middleware};
use nora_provider_codex_history::Plugin;
use serde_json::{json, Value};
fn options() -> Value {
    json!({"providers":{"magpie:demo":{"historyFilter":true,"preserveEncryptedInclude":true}}})
}
fn request() -> Value {
    json!({"model":"demo/mock-model","include":["reasoning.encrypted_content"],"reasoning":{"effort":"xhigh"},"instructions":"Summarize the recorded history.","input":[{"type":"reasoning","id":"rs_foreign","encrypted_content":"FOREIGN_CIPHERTEXT","summary":[]},{"type":"web_search_call","id":"ws_foreign","status":"completed","action":{"type":"search","query":"example","queries":["example","another example"],"sources":[{"type":"url","url":"https://example.invalid/rules","title":"Recorded source"}]},"internal_chat_message_metadata_passthrough":{"turn_id":"PRIVATE_TURN"}},{"type":"message","role":"assistant","content":"Recorded conclusion: https://example.invalid/rules"},{"type":"function_call","id":"fc_old","call_id":"pair_keep","name":"read_file","arguments":"{\"id\":\"BUSINESS_ID\"}"},{"type":"function_call_output","id":"fco_old","call_id":"pair_keep","output":"FILE_TEXT_KEEP"},{"role":"user","content":"Create a text summary without new searches."}]})
}
fn context() -> Context {
    Context {
        protocol: "responses".into(),
        path: "/v1/responses".into(),
        options: options(),
        state: json!({}),
        ..Default::default()
    }
}
fn filter(body: &Value, context: &mut Context) -> (Action, Value) {
    let mut next = body.clone();
    let result = Plugin.apply("request", &mut next, context).unwrap();
    (result, next)
}
fn record(value: &Value) -> Value {
    serde_json::from_str(
        value["content"]
            .as_str()
            .unwrap()
            .split_once('\n')
            .unwrap()
            .1,
    )
    .unwrap()
}
#[test]
fn search_history_retains_records_tool_pairs_and_options_without_foreign_ciphertext() {
    let original = request();
    let (_, result) = filter(&original, &mut context());
    assert_eq!(
        record(&result["input"][0]),
        json!({"status":original["input"][1]["status"],"action":original["input"][1]["action"]})
    );
    let encoded = result.to_string();
    for hidden in [
        "FOREIGN_CIPHERTEXT",
        "rs_foreign",
        "ws_foreign",
        "PRIVATE_TURN",
    ] {
        assert!(!encoded.contains(hidden));
    }
    assert_eq!(result["input"][1], original["input"][2]);
    for (i, k) in [(2, "arguments"), (3, "output")] {
        assert_eq!(result["input"][i]["call_id"], "pair_keep");
        assert_eq!(result["input"][i][k], original["input"][i + 1][k]);
        assert!(result["input"][i].get("id").is_none());
    }
    for key in ["reasoning", "include", "instructions", "model"] {
        assert_eq!(result[key], original[key]);
    }
    assert_eq!(result["store"], false);
    assert_eq!(filter(&result, &mut context()).1, result);
    assert_eq!(original, request());
}
#[test]
fn all_search_actions_and_partial_states_remain_recorded_data() {
    for action in [
        json!({"type":"search","query":"example"}),
        json!({"type":"open_page","url":"https://example.invalid/doc","sources":[{"id":"BUSINESS_ID"}]}),
        json!({"type":"find_in_page","url":"https://example.invalid/doc","pattern":"deadline"}),
    ] {
        for status in ["completed", "in_progress", "incomplete"] {
            let mut body = request();
            body["input"] =
                json!([{"type":"web_search_call","id":"ws_old","status":status,"action":action}]);
            assert_eq!(
                record(&filter(&body, &mut context()).1["input"][0]),
                json!({"status":status,"action":action})
            );
        }
    }
    let mut body = request();
    body["input"] = json!([{"type":"web_search_call"}]);
    assert_eq!(
        record(&filter(&body, &mut context()).1["input"][0]),
        json!({})
    );
}
#[test]
fn only_enabled_responses_rules_and_explicit_aliases_apply() {
    for options in [
        json!({}),
        json!({"providers":{"magpie:demo":{"historyFilter":false}}}),
    ] {
        let mut c = context();
        c.options = options;
        assert_eq!(filter(&request(), &mut c).0, Action::Keep);
    }
    for protocol in ["chat-completions", "anthropic"] {
        let mut c = context();
        c.protocol = protocol.into();
        assert_eq!(filter(&request(), &mut c).0, Action::Keep);
    }
    for model in ["other/mock-model", "mock-model"] {
        let mut r = request();
        r["model"] = json!(model);
        assert_eq!(filter(&r, &mut context()).0, Action::Keep);
    }
    let mut c = context();
    c.options["aliases"] = json!({"old-demo":"demo"});
    let mut r = request();
    r["model"] = json!("old-demo/mock-model");
    assert_eq!(
        record(&filter(&r, &mut c).1["input"][0]),
        record(&filter(&request(), &mut context()).1["input"][0])
    );
}
#[test]
fn nonportable_native_tools_keep_resource_ids_and_contents() {
    let item = json!({"type":"file_search_call","id":"fs_keep","queries":["example"],"results":[{"id":"resource_keep","text":"FILE_TEXT"}]});
    let mut body = request();
    body["input"] = json!([item]);
    assert_eq!(filter(&body, &mut context()).1["input"], body["input"]);
}
#[test]
fn unsafe_context_rejects_and_only_explicit_encrypted_parts_are_removed() {
    let mut c = context();
    c.path = "/v1/responses/compact".into();
    assert!(matches!(
        filter(&request(), &mut c).0,
        Action::Reject { status: 409, .. }
    ));
    for (key, value) in [
        ("previous_response_id", json!("resp_old")),
        ("conversation", json!("conv_old")),
        (
            "input",
            json!([{"type":"compaction","encrypted_content":"SEALED_CONTEXT"}]),
        ),
        ("input", json!([{"type":"item_reference","id":"ws_old"}])),
        ("context_management", json!([{"type":"compaction"}])),
    ] {
        let mut r = request();
        r[key] = value;
        assert!(matches!(
            filter(&r, &mut context()).0,
            Action::Reject { .. }
        ));
    }
    let mut body = request();
    body["input"][1]["content"] = json!([{"type":"encrypted_content","encrypted_content":"SEALED_PART"},{"type":"input_text","text":"RECORDED_TEXT"}]);
    let (_, result) = filter(&body, &mut context());
    assert!(!result.to_string().contains("SEALED_PART"));
    assert!(result.to_string().contains("RECORDED_TEXT"));
}
#[test]
fn package_exposes_request_only_and_has_no_provider_or_network_logic() {
    let manifest: Value = serde_json::from_str(include_str!("../nora-plugin.json")).unwrap();
    let declaration = &manifest["contributes"]["middleware"];
    assert_eq!(declaration["request"], true);
    for key in ["provider", "event", "response"] {
        assert!(declaration.get(key).is_none());
    }
    assert!(manifest["contributes"].get("provider").is_none());
    let source = include_str!("../src/lib.rs");
    assert!(!source.contains("reqwest"));
    assert!(!source.contains("HttpRequest"));
}
