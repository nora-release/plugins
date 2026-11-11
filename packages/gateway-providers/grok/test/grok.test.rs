use super::*;

#[test]
fn requests_are_signed_as_grok_build() {
    let request = sign(
        HttpRequest::new(provider_support::HttpMethod::Get, BASE),
        "t1",
        "1.0.77",
    );
    let header = |name: &str| request.headers.get(name).map(String::as_str);
    assert_eq!(header("authorization"), Some("Bearer t1"));
    assert_eq!(header("x-xai-token-auth"), Some("xai-grok-cli"));
    assert_eq!(header("x-grok-client-identifier"), Some("grok-shell"));
    assert_eq!(header("x-grok-client-mode"), Some("headless"));
    assert_eq!(header("x-grok-client-version"), Some("1.0.77"));
    assert!(header("user-agent")
        .unwrap()
        .starts_with("grok-shell/1.0.77 (macos; "));
}

#[test]
fn the_stable_channel_names_a_version() {
    assert_eq!(version("1.0.77\n").as_deref(), Some("1.0.77"));
    assert_eq!(version("1.1.0-beta.2").as_deref(), Some("1.1.0-beta.2"));
    assert_eq!(version("<html>"), None);
    assert_eq!(version("1.0"), None);
}

#[test]
fn a_window_suffix_names_the_window_and_fast_the_fast_tier() {
    let of = |id: &str| {
        let (model, window) = model_of(id);
        (model, window)
    };
    assert_eq!(of("grok-4.7"), ("grok-4.7".into(), 0));
    assert_eq!(of("grok-4.7@500k"), ("grok-4.7".into(), 500_000));
    assert_eq!(of("grok-4.7@131072"), ("grok-4.7".into(), 131_072));
    assert_eq!(of("odd@name"), ("odd@name".into(), 0));
    assert_eq!(of("grok-4.7+fast"), ("grok-4.7-build-fast".into(), 0));
    assert_eq!(
        of("grok-4.7+fast@500k"),
        ("grok-4.7-build-fast".into(), 500_000)
    );
    assert_eq!(of("+fast"), ("+fast".into(), 0));
}

#[test]
fn a_models_fast_tier_is_its_fast_option() {
    let listed = models(&json!({"data": [
        {"id": "grok-4.7", "name": "Grok 4.7", "context_window": 256000, "context_windows": [256000, 500000]},
        {"id": "grok-4.7-build-fast", "name": "Grok 4.7 Fast", "context_window": 256000, "context_windows": [256000, 500000]},
        {"id": "grok-9-build-fast", "context_window": 128000}
    ]}));
    let ids: Vec<&str> = listed.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "grok-4.7",
            "grok-4.7@500k",
            "grok-4.7+fast",
            "grok-4.7+fast@500k",
            "grok-9-build-fast"
        ]
    );
    assert_eq!(listed[2].name.as_deref(), Some("Grok 4.7 Fast"));
    assert_eq!(listed[3].context, Some(500_000));
}

#[test]
fn each_listed_window_is_a_model() {
    let listed = models(&json!({"data": [
        {"id": "grok-4.7", "name": "Grok 4.7", "context_window": 256000, "context_windows": [256000, 500000, 2000000],
         "reasoning_efforts": [{"value": "high"}, {"value": "low"}], "input_modalities": ["text", "image"]},
        {"id": "grok-code", "context_window": 128000},
        {"id": "grok-imagine", "api_backend": "images"}
    ]}));
    let ids: Vec<&str> = listed.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(
        ids,
        ["grok-4.7", "grok-4.7@500k", "grok-4.7@2000k", "grok-code"]
    );
    assert_eq!(listed[0].context, Some(256_000));
    assert!(listed[0].reasoning && listed[0].images);
    assert_eq!(listed[1].name.as_deref(), Some("Grok 4.7 @ 500k"));
    assert_eq!(listed[1].context, Some(500_000));
    assert_eq!(listed[3].name.as_deref(), Some("grok-code"));
    assert!(!listed[3].reasoning);
}

fn rewritten(body: Value) -> (Value, bool) {
    let mut body = body;
    let changed = rewrite(body.as_object_mut().unwrap());
    (body, changed)
}

#[test]
fn what_grok_turns_away_is_left_out() {
    let (body, changed) = rewritten(json!({
        "model": "grok-4.7",
        "tools": [
            {"type": "function", "name": "read", "parameters": {"type": "object"}},
            {"type": "custom", "name": "apply_patch"},
            {"type": "web_search", "external_web_access": true}
        ],
        "tool_choice": {"type": "custom", "name": "apply_patch"},
        "input": [{"type": "reasoning", "content": null, "encrypted_content": "e"}],
        "include": ["reasoning.encrypted_content", "output.encrypted_content"]
    }));
    assert!(changed);
    assert_eq!(body["tools"].as_array().unwrap().len(), 2);
    assert!(body["tools"][1].get("external_web_access").is_none());
    assert!(body.get("tool_choice").is_none());
    assert!(body["input"][0].get("content").is_none());
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
}

#[test]
fn a_tool_choice_without_tools_goes() {
    let (body, changed) = rewritten(json!({"tool_choice": "none", "input": "hi"}));
    assert!(changed);
    assert!(body.get("tool_choice").is_none());
    let (_, changed) = rewritten(json!({"input": "hi"}));
    assert!(!changed);
}

#[test]
fn an_agent_message_goes_as_the_users() {
    let (body, _) = rewritten(json!({"input": [{
        "type": "agent_message", "author": "lead", "recipient": "worker",
        "content": [{"type": "input_text", "text": "do it"}]
    }]}));
    assert_eq!(body["input"][0]["role"], "user");
    assert_eq!(
        body["input"][0]["content"][0]["text"],
        "From lead to worker\n\n"
    );
    assert_eq!(body["input"][0]["content"][1]["text"], "do it");
}

#[test]
fn a_union_at_a_tools_root_is_folded_to_an_object() {
    let folded = object_root(&json!({
        "anyOf": [
            {"type": "object", "properties": {"mode": {"const": "create"}, "name": {"type": "string"}}, "required": ["mode", "name"]},
            {"$ref": "#/$defs/update"}
        ],
        "$defs": {"update": {"type": "object", "properties": {"mode": {"const": "update"}, "id": {"type": "string"}}, "required": ["mode", "id"]}}
    }));
    assert_eq!(folded["type"], "object");
    assert!(folded.get("anyOf").is_none());
    assert_eq!(folded["required"], json!(["mode"]));
    assert_eq!(
        folded["properties"]["mode"]["anyOf"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(folded["properties"]["name"], json!({"type": "string"}));
    let plain = json!({"type": "object", "properties": {}});
    assert_eq!(object_root(&plain), plain);
}

#[test]
fn usage_reads_the_periods_credits_and_on_demand() {
    let read = usage(&json!({"config": {
        "creditUsagePercent": 42.5,
        "currentPeriod": {"type": "USAGE_PERIOD_TYPE_WEEKLY", "end": "2026-11-16T00:00:00Z"},
        "onDemandCap": {"val": 20}, "onDemandUsed": {"val": 5}
    }}));
    assert_eq!(read.windows[0].name, "7 days");
    assert_eq!(read.windows[0].used, Some(42.5));
    assert_eq!(
        read.windows[0].resets_at.as_deref(),
        Some("2026-11-16T00:00:00Z")
    );
    assert_eq!(read.windows[1].name, "On-demand");
    assert_eq!(read.windows[1].used, Some(25.0));
    let bare = usage(&json!({"config": {}}));
    assert_eq!(bare.windows.len(), 1);
    assert_eq!(bare.windows[0].name, "Allowance");
}

#[test]
fn a_models_efforts_are_weakest_first_with_its_default() {
    let listed = models(&json!({"data": [{
        "id": "grok-4.7", "context_window": 256000, "description": "Frontier.",
        "reasoning_efforts": [
            {"value": "xhigh", "default": false}, {"value": "high", "default": true},
            {"value": "medium", "default": false}, {"value": "low", "default": false}
        ]
    }]}));
    assert_eq!(listed[0].efforts, ["low", "medium", "high", "xhigh"]);
    assert_eq!(listed[0].default_effort.as_deref(), Some("high"));
    assert_eq!(listed[0].description.as_deref(), Some("Frontier."));
    assert!(listed[0].reasoning);
}
