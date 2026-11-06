use super::*;
use serde_json::json;

const PROMPT: &str = "You are Codex, a coding agent.\nNative model instructions.";

fn finalized(oauth: bool, target: &str, original: &Value) -> Value {
    let mut body = Some(serde_json::to_vec(original).unwrap());
    responses_body::finalize_with_prompt(
        oauth,
        &Url::parse(target).unwrap(),
        &mut Default::default(),
        &mut body,
        |_| PROMPT.into(),
    );
    serde_json::from_slice(&body.unwrap()).unwrap()
}

fn prepared(
    original: &Value,
    mut headers: BTreeMap<String, String>,
) -> (Value, BTreeMap<String, String>) {
    let mut body = Some(serde_json::to_vec(original).unwrap());
    responses_body::finalize_with_prompt(
        true,
        &Url::parse("https://chatgpt.com/backend-api/codex/responses").unwrap(),
        &mut headers,
        &mut body,
        |_| PROMPT.into(),
    );
    (serde_json::from_slice(&body.unwrap()).unwrap(), headers)
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
        for key in ["background", "include", "client_metadata"] {
            assert_eq!(body[key], original[key]);
        }
        assert_eq!(body["input"][0]["content"][0]["text"], "hi");
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

#[test]
fn magpie_reference_body_and_session_headers_match_for_a_plain_turn() {
    let original = json!({
        "model":"gpt-5.5", "input":"hi", "max_output_tokens":5,
        "max_completion_tokens":7, "temperature":0.1, "top_p":0.9,
        "previous_response_id":"stored-response", "user":"caller",
        "safety_identifier":"caller", "prompt_cache_options":{"mode":"explicit"},
        "stream":false, "store":true, "reasoning":{"effort":"low"}
    });
    let (actual, headers) = prepared(
        &original,
        BTreeMap::from([
            ("Session_Id".into(), "old-session".into()),
            ("CONVERSATION_ID".into(), "old-conversation".into()),
            ("SESSION-ID".into(), "caller-session".into()),
            ("Thread-Id".into(), "caller-thread".into()),
            ("Accept".into(), "application/json".into()),
        ]),
    );
    // The reference Go conversationKey hashes the normalized first user item.
    let key = "c12dc832-a353-6f7c-97db-6d52e8e861f1";
    assert_eq!(
        actual,
        json!({
            "model":"gpt-5.5", "instructions":PROMPT,
            "input":[{"type":"message", "role":"user", "content":[{"type":"input_text", "text":"hi"}]}],
            "stream":true, "store":false, "tool_choice":"auto", "parallel_tool_calls":true,
            "text":{"verbosity":"medium"}, "reasoning":{"effort":"low", "summary":"auto"},
            "include":["reasoning.encrypted_content"], "prompt_cache_key":key
        })
    );
    assert_eq!(
        headers,
        BTreeMap::from([
            ("accept".into(), "text/event-stream".into()),
            ("session-id".into(), key.into()),
            ("thread-id".into(), key.into()),
        ])
    );
}

#[test]
fn instructions_move_to_developer_without_losing_native_tools_or_call_outputs() {
    let original = json!({
        "model":"gpt-5.5", "instructions":"You are Pi.", "reasoning":{"effort":"ultra"},
        "tools":[{"type":"function", "name":"shell", "parameters":{"type":"object"}}],
        "tool_choice":{"type":"function", "name":"shell"}, "parallel_tool_calls":false,
        "input":[
            {"type":"message", "role":"system", "content":"sys", "id":"old-system", "status":"completed"},
            {"type":"item_reference", "id":"msg-1"},
            {"type":"message", "id":"msg-2", "role":"user", "content":"hi"},
            {"type":"function_call", "id":"fc-1", "call_id":"c1", "name":"ls", "arguments":"{}", "status":"completed"},
            {"type":"function_call_output", "call_id":"c1", "output":"a"},
            {"type":"function_call_output", "call_id":"c9", "name":"cat", "output":"b"},
            {"type":"reasoning", "id":"rs-1", "summary":[], "encrypted_content":"preserved", "status":"completed"}
        ]
    });
    let (actual, _) = prepared(&original, BTreeMap::new());
    assert_eq!(actual["instructions"], PROMPT);
    assert_eq!(
        actual["input"],
        json!([
            {"type":"message", "role":"developer", "content":[{"type":"input_text", "text":"You are Pi."}]},
            {"type":"message", "role":"developer", "content":"sys"},
            {"type":"message", "role":"user", "content":"hi"},
            {"type":"function_call", "call_id":"c1", "name":"ls", "arguments":"{}", "status":"completed"},
            {"type":"function_call_output", "call_id":"c1", "output":"a"},
            {"type":"message", "role":"assistant", "content":"[Previous cat result; call_id=c9]: b"},
            {"type":"reasoning", "summary":[], "encrypted_content":"preserved"}
        ])
    );
    assert_eq!(actual["tools"], original["tools"]);
    assert_eq!(actual["tool_choice"], original["tool_choice"]);
    assert_eq!(actual["parallel_tool_calls"], false);
    assert_eq!(
        actual["reasoning"],
        json!({"effort":"max", "summary":"auto"})
    );
}

#[test]
fn native_codex_instructions_and_explicit_thread_keys_are_preserved() {
    let original = json!({
        "instructions":"  You are Codex, a coding agent.\nCaller personality and policies.\n",
        "prompt_cache_key":"existing-thread", "input":[{"role":"user", "content":"hi"}],
        "tool_choice":"required", "text":{"verbosity":"low"}
    });
    let (actual, headers) = prepared(&original, BTreeMap::new());
    assert_eq!(actual["instructions"], original["instructions"]);
    assert_eq!(actual["input"], original["input"]);
    assert_eq!(actual["tool_choice"], "required");
    assert_eq!(actual["text"], original["text"]);
    assert_eq!(headers["session-id"], "existing-thread");
    assert_eq!(headers["thread-id"], "existing-thread");
}

#[test]
fn cache_keys_match_go_object_order_html_unicode_and_large_integer_encoding() {
    let original = json!({
        "instructions":"Rules <a> & b\u{2028}.",
        "input":[
            {"role":"developer", "type":"message", "content":{"z":9007199254740993_u64, "a":"<&>\u{2029}"}},
            {"content":[{"type":"input_text", "text":"fix <bug> & test\u{2028}"}], "role":"user", "type":"message"}
        ]
    });
    let (actual, _) = prepared(&original, BTreeMap::new());
    assert_eq!(
        actual["prompt_cache_key"],
        "add95f2b-4653-fe89-6bb5-7ca491d00f86"
    );
    let reordered: Value = serde_json::from_str(
        r#"{"input":[{"content":{"a":"<&>\u2029","z":9007199254740993},"type":"message","role":"developer"},{"type":"message","role":"user","content":[{"text":"fix <bug> & test\u2028","type":"input_text"}]}],"instructions":"Rules <a> & b\u2028."}"#,
    ).unwrap();
    assert_eq!(
        prepared(&reordered, BTreeMap::new()).0["prompt_cache_key"],
        actual["prompt_cache_key"]
    );
}

#[test]
fn cache_keys_and_session_headers_stay_stable_across_tool_and_later_turns() {
    let first = json!({"instructions":"You are Pi.", "input":[{"type":"message", "role":"user", "content":"hi"}]});
    let (initial, headers) = prepared(&first, BTreeMap::new());
    assert_eq!(
        initial["prompt_cache_key"],
        "66655d0e-4276-e0ac-9052-545c071e26c3"
    );
    let mut later = first.clone();
    later["input"].as_array_mut().unwrap().extend([
        json!({"type":"function_call", "call_id":"c1", "name":"ls", "arguments":"{}"}),
        json!({"type":"function_call_output", "call_id":"c1", "output":"a"}),
        json!({"type":"message", "role":"user", "content":"and the test?"}),
    ]);
    let (continued, continued_headers) = prepared(&later, BTreeMap::new());
    assert_eq!(continued["prompt_cache_key"], initial["prompt_cache_key"]);
    assert_eq!(continued_headers, headers);
    let (different, _) = prepared(
        &json!({"instructions":"You are Pi.", "input":"bye"}),
        BTreeMap::new(),
    );
    assert_ne!(different["prompt_cache_key"], initial["prompt_cache_key"]);
}

#[test]
fn keyless_requests_keep_modern_session_headers_and_drop_old_names() {
    let (actual, headers) = prepared(
        &json!({"input":[]}),
        BTreeMap::from([
            ("session-id".into(), "caller-session".into()),
            ("thread-id".into(), "caller-thread".into()),
            ("session_id".into(), "old-session".into()),
            ("conversation_id".into(), "old-conversation".into()),
        ]),
    );
    assert!(actual.get("prompt_cache_key").is_none());
    assert_eq!(headers["session-id"], "caller-session");
    assert_eq!(headers["thread-id"], "caller-thread");
    assert!(!headers.keys().any(|name| name.contains('_')));
    for key in ["injected\r\nheader", "\tbad"] {
        let (_, headers) = prepared(&json!({"prompt_cache_key":key}), BTreeMap::new());
        assert!(!headers.contains_key("session-id"));
    }
}

#[test]
fn standalone_notifications_keep_their_native_name_namespace_and_item_id() {
    for kind in ["function_call_output", "custom_tool_call_output"] {
        for id in [None, Some(json!("")), Some(Value::Null)] {
            let mut notification = json!({"type":kind, "id":"standalone", "name":"notify", "namespace":"nora", "output":"notice"});
            if let Some(id) = id {
                notification["call_id"] = id;
            }
            let (actual, _) = prepared(&json!({"input":[notification.clone()]}), BTreeMap::new());
            assert_eq!(actual["input"][0], notification);
        }
    }
}

#[test]
fn long_foreign_tool_ids_are_bounded_without_breaking_call_result_pairs() {
    for kind in ["function", "custom_tool", "local_shell"] {
        let long = format!("call-{}-46__fc_{}_0", "a".repeat(36), "b".repeat(36));
        let other = format!("{}1", &long[..long.len() - 1]);
        let short = "x".repeat(64);
        let mut items = Vec::new();
        for id in [&long, &other, &short] {
            items.push(json!({"type":format!("{kind}_call"), "call_id":id, "name":"shell", "arguments":"{}"}));
            items.push(json!({"type":format!("{kind}_call_output"), "call_id":id, "output":"ok"}));
        }
        let original = json!({"input":items});
        let (actual, _) = prepared(&original, BTreeMap::new());
        let input = actual["input"].as_array().unwrap();
        assert_eq!(input.len(), 6);
        assert_eq!(input[0]["call_id"].as_str().unwrap().len(), 64);
        assert_eq!(input[0]["call_id"], input[1]["call_id"]);
        assert_eq!(input[2]["call_id"], input[3]["call_id"]);
        assert_ne!(input[0]["call_id"], input[2]["call_id"]);
        assert_eq!(input[4]["call_id"], short);
        assert_eq!(input[5]["call_id"], short);
        assert_eq!(
            prepared(&original, BTreeMap::new()).0["input"],
            actual["input"]
        );
    }
}

#[test]
fn orphan_outputs_are_bounded_on_utf8_boundaries_and_keep_structured_values() {
    let (actual, _) = prepared(
        &json!({"input":[
            {"type":"function_call_output", "call_id":"missing", "output":"猫".repeat(6000)},
            {"type":"function_call_output", "call_id":"missing-json", "output":{"n":9007199254740993_u64, "a":"<&>"}}
        ]}),
        BTreeMap::new(),
    );
    let text = actual["input"][0]["content"].as_str().unwrap();
    assert!(text.ends_with("\n...[truncated]"));
    assert_eq!(text.matches('猫').count(), 5333);
    assert!(actual["input"][1]["content"]
        .as_str()
        .unwrap()
        .contains(r#"{"a":"\u003c\u0026\u003e","n":9007199254740993}"#));
}

#[test]
fn replayed_search_uses_its_existing_native_or_client_tool_declaration() {
    let search = json!({"type":"web_search_call", "status":"completed", "action":{"type":"search", "query":"go release"}});
    for tool in [
        json!({"type":"web_search_preview"}),
        json!({"type":"function", "name":"web.run"}),
        json!({"type":"namespace", "name":"web", "tools":[{"type":"function", "name":"run"}]}),
    ] {
        let (actual, _) = prepared(
            &json!({"tools":[tool.clone()], "input":[search.clone()]}),
            BTreeMap::new(),
        );
        assert_eq!(actual["tools"], json!([tool.clone()]));
        let (lite, _) = prepared(
            &json!({"input":[{"type":"additional_tools", "tools":[tool.clone()]}, search.clone()]}),
            BTreeMap::new(),
        );
        assert_eq!(lite["input"][0]["tools"], json!([tool]));
        assert!(lite.get("tools").is_none());
    }
}

#[test]
fn replayed_search_adds_only_cached_search_and_preserves_tool_choice_constraints() {
    let search =
        json!({"type":"web_search_call", "action":{"type":"search", "query":"go release"}});
    let cached = json!({"type":"web_search", "external_web_access":false});
    for choice in [
        json!("auto"),
        json!("none"),
        json!("required"),
        json!({"type":"function", "name":"shell"}),
    ] {
        let (actual, _) = prepared(
            &json!({"tool_choice":choice, "input":[search.clone()]}),
            BTreeMap::new(),
        );
        assert_eq!(actual["tools"], json!([cached.clone()]));
        assert_eq!(
            actual["tool_choice"],
            if choice == "auto" {
                json!("none")
            } else {
                choice
            }
        );
    }
    let (lite, _) = prepared(
        &json!({"reasoning":{"effort":"low", "context":"all_turns"}, "input":[search.clone()]}),
        BTreeMap::new(),
    );
    assert!(lite.get("tools").is_none());
    assert_eq!(
        lite["input"][0],
        json!({"type":"additional_tools", "role":"developer", "tools":[cached.clone()]})
    );
    let tools = json!([{"type":"namespace", "name":"functions", "tools":[{"type":"function", "name":"run"}]}]);
    let (additional, _) = prepared(
        &json!({"input":[{"type":"additional_tools", "role":"developer", "tools":tools.clone()}, search]}),
        BTreeMap::new(),
    );
    assert_eq!(additional["input"][0]["tools"], json!([tools[0], cached]));
    assert_eq!(additional["tool_choice"], "auto");
}
