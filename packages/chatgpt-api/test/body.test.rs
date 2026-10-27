use super::*;

#[test]
fn strips_subscription_fields_but_preserves_agent_instructions_and_own_tools() {
    let mut input = json!({"model":"gpt-6-sol","instructions":"You are my agent.","store":true,"stream":false,
        "input":[{"role":"system","content":"be brief"},{"role":"user","content":"hi"},
        {"type":"additional_tools","role":"developer","tools":[{"type":"tool_search"},{"type":"namespace","name":"functions","tools":[]}]}],
        "tools":[{"type":"function","name":"shell","parameters":{"type":"object"}},{"type":"image_generation"},{"type":"web_search"}],
        "tool_choice":"auto","prompt_cache_key":"client-thread","reasoning":{"effort":"high"}});
    for key in DROPPED {
        input[*key] = json!("refused");
    }
    let out: Value =
        serde_json::from_slice(&prepare(&serde_json::to_vec(&input).unwrap())).unwrap();
    for key in DROPPED {
        assert!(out.get(*key).is_none(), "{key}");
    }
    assert_eq!(out["instructions"], "You are my agent.");
    assert_eq!(out["store"], false);
    assert_eq!(out["stream"], true);
    assert_eq!(out["prompt_cache_key"], "client-thread");
    assert_eq!(out["reasoning"], json!({"effort":"high"}));
    assert_eq!(out["input"][0]["role"], "developer");
    assert_eq!(out["input"][2]["tools"].as_array().unwrap().len(), 1);
    assert_eq!(out["tools"].as_array().unwrap().len(), 2);
    assert_eq!(out["tools"][0]["name"], "shell");
    assert_eq!(out["tools"][1]["type"], "web_search");
    assert_eq!(out["tool_choice"], "auto");
}
#[test]
fn string_input_and_hosted_only_tools_become_a_stateless_user_message() {
    let out: Value = serde_json::from_slice(&prepare(
        br#"{"input":"hello","tools":[{"type":"file_search"}],"tool_choice":"auto"}"#,
    ))
    .unwrap();
    assert_eq!(
        out["input"],
        json!([{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}])
    );
    assert!(out.get("tools").is_none());
    assert!(out.get("tool_choice").is_none());
    assert_eq!(prepare(b"not JSON"), b"not JSON");
    assert_eq!(prepare(b"[]"), b"[]");
}
#[test]
fn conversation_key_is_stable_across_turns_and_matches_go_html_encoding() {
    let user = json!({"type":"message","role":"user","content":[{"type":"input_text","text":"<fix>&\u{2028}"}]});
    let first = conversation_key("You are my agent.", &[user.clone()]).unwrap();
    assert_eq!(
        first,
        conversation_key(
            "You are my agent.",
            &[
                user.clone(),
                json!({"role":"assistant","content":"done"}),
                json!({"role":"user","content":"test?"})
            ]
        )
        .unwrap()
    );
    let encoded = r#"{"content":[{"text":"\u003cfix\u003e\u0026\u2028","type":"input_text"}],"role":"user","type":"message"}"#;
    let digest = format!(
        "{:x}",
        Sha256::digest(format!("You are my agent.{encoded}").as_bytes())
    );
    assert_eq!(first.replace('-', ""), digest[..32]);
    assert_ne!(first, conversation_key("other", &[user]).unwrap());
    assert!(conversation_key("", &[json!({"role":"developer"})]).is_none());
}
#[test]
fn explains_shared_usage_eligibility_capability_and_reauthentication() {
    for (code, needle) in [
        (
            "subscription_sharing_usage_limit_exceeded",
            "https://chatgpt.com/settings/usage",
        ),
        ("subscription_sharing_usage_unavailable", "moment"),
        ("subscription_sharing_user_unavailable", "moment"),
        ("subscription_sharing_user_not_eligible", "not eligible"),
        ("subscription_sharing_unsupported_capability", "capability"),
        ("subscription_sharing_route_not_supported", "endpoint"),
        ("subscription_sharing_invalid_user", "Sign in again"),
        ("chatpass_v2_invalid_user", "chatpass_v2_invalid_user"),
    ] {
        let value = json!({"error":{"code":code,"param":"capability"}});
        assert!(explain(400, &value).unwrap().contains(needle), "{code}");
    }
    assert!(explain(401, &json!({})).unwrap().contains("Sign in again"));
    assert!(explain(503, &json!({})).is_none());
    assert!(explain(
        429,
        &json!({"detail":"usage refused: subscription_sharing_usage_limit_exceeded."})
    )
    .unwrap()
    .contains("usage limit"));
    assert!(explain(400,&json!({"detail":{"code":"subscription_sharing_unsupported_capability","param":"image_generation"}})).unwrap().contains("image_generation"));
}

#[test]
fn conversation_key_keeps_previous_nested_key_order() {
    let a: Value =
        serde_json::from_str(r#"{"role":"user","content":[{"type":"text","text":"x"}]}"#).unwrap();
    let b: Value =
        serde_json::from_str(r#"{"content":[{"text":"x","type":"text"}],"role":"user"}"#).unwrap();
    assert_eq!(
        conversation_key("system", &[a]),
        conversation_key("system", &[b])
    );
}
