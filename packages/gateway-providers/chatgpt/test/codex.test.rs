use super::*;

#[test]
fn a_responses_body_is_made_one_the_codex_backend_takes() {
    let mut body = json!({
        "model": "gpt-5.5", "input": "hi", "instructions": "You are terse.\nAlways.", "max_output_tokens": 10,
        "temperature": 0.3, "previous_response_id": "r", "stream": false, "reasoning": {"effort": "ultra"}
    });
    let object = body.as_object_mut().unwrap();
    super::body(object, Some("You are Codex.\nOfficial.".into()));
    for gone in ["max_output_tokens", "temperature", "previous_response_id"] {
        assert!(!object.contains_key(gone), "{gone} kept");
    }
    assert_eq!(object["instructions"], "You are Codex.\nOfficial.");
    assert_eq!(object["input"][0]["role"], "developer");
    assert_eq!(
        object["input"][0]["content"][0]["text"],
        "You are terse.\nAlways."
    );
    assert_eq!(object["input"][1]["content"][0]["text"], "hi");
    assert_eq!(
        (object["store"].clone(), object["stream"].clone()),
        (json!(false), json!(true))
    );
    assert_eq!(
        object["reasoning"],
        json!({"effort": "max", "summary": "auto"})
    );
    assert_eq!(object["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(object["tool_choice"], "auto");

    // the caller's own copy of the official prompt stands in for it; no reasoning, no include
    let mut same = json!({"input": [{"type": "item_reference", "id": "x"}], "instructions": "You are Codex.\nchanged", "reasoning": {"effort": "none"}, "include": ["x"]});
    let object = same.as_object_mut().unwrap();
    super::body(object, Some("You are Codex.\nOfficial.".into()));
    assert_eq!(object["instructions"], "You are Codex.\nchanged");
    assert_eq!(object["input"], json!([]));
    assert!(!object.contains_key("include"));
}

#[test]
fn the_model_list_keeps_its_order_and_each_models_instructions() {
    let listed = models(&json!({"models": [
        {"slug": "b", "priority": 2, "display_name": "B", "context_window": 272000, "supported_reasoning_levels": [{"effort": "low"}], "input_modalities": ["text", "image"], "base_instructions": "prompt b"},
        {"slug": "hidden", "priority": 0, "visibility": "hide"},
        {"slug": "a", "priority": 1, "model_messages": {"instructions_template": "prompt a"}},
    ]})).unwrap();
    let ids = listed
        .iter()
        .map(|entry| entry.model.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["a", "b"]);
    assert_eq!(listed[0].instructions.as_deref(), Some("prompt a"));
    let b = &listed[1].model;
    assert_eq!(
        (b.name.as_deref(), b.context, b.reasoning, b.images),
        (Some("B"), Some(272000), true, true)
    );
    assert!(models(&json!({})).is_err());
}

#[test]
fn usage_reads_both_windows_and_the_credits() {
    let usage = usage(&json!({"plan_type": "prolite", "rate_limit": {
        "primary_window": {"used_percent": 12.5, "limit_window_seconds": 18000, "reset_at": 1_900_000_000u64},
        "secondary_window": {"used_percent": 100.0, "limit_window_seconds": 604800, "reset_after_seconds": 60}
    }, "credits": {"has_credits": true, "balance": "56107.7"}}));
    assert_eq!(usage.plan.as_deref(), Some("prolite"));
    let names = usage
        .windows
        .iter()
        .map(|window| window.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["5 hours", "7 days", "Credits"]);
    assert_eq!(usage.windows[0].used, Some(12.5));
    assert_eq!(
        usage.windows[0].resets_at.as_deref(),
        Some("2030-03-17T17:46:40Z")
    );
    assert!(usage.windows[1].resets_at.is_some());
    assert_eq!(usage.windows[2].balance, Some(56107.7));
}

#[test]
fn a_stream_ends_on_its_completed_response() {
    let stream = "event: response.created\ndata: {\"type\":\"response.created\"}\n\n\
event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"output\":[]}}\n\n";
    assert_eq!(completed(stream), Some(json!({"id": "r1", "output": []})));
    assert_eq!(completed("data: {\"type\":\"response.created\"}\n\n"), None);
    assert_eq!(
        explain(429, &json!({"detail": "Slow down"})),
        "ChatGPT answered HTTP 429: Slow down"
    );
}

#[test]
fn usage_counts_the_resets_and_the_list_names_each() {
    let usage =
        usage(&json!({"plan_type": "plus", "rate_limit_reset_credits": {"available_count": 2}}));
    let resets = usage.resets.unwrap();
    assert_eq!(resets.count, 2);
    assert!(resets.cards.is_empty());
    assert!(
        super::usage(&json!({"rate_limit_reset_credits": {"available_count": 0}}))
            .resets
            .is_none()
    );

    let cards = reset_cards(&json!({"available_count": 2, "credits": [
        {"id": "late", "status": "available", "expires_at": "2027-01-01T00:00:00Z"},
        {"id": "spent", "status": "redeemed", "expires_at": "2026-11-20T00:00:00Z"},
        {"id": "never", "status": "available", "expires_at": null},
        {"id": "soon", "status": "available", "expires_at": "2026-12-02T06:56:00Z"}
    ]}));
    let ids: Vec<&str> = cards.iter().filter_map(|card| card.id.as_deref()).collect();
    assert_eq!(ids, ["soon", "late", "never"]);
    assert_eq!(cards[0].expires_at.as_deref(), Some("2026-12-02T06:56:00Z"));
    assert!(cards[2].expires_at.is_none());
}

#[test]
fn a_models_efforts_and_its_fast_tier_are_listed() {
    let listed = models(&json!({"models": [{
        "slug": "gpt-6.1-sol", "display_name": "GPT-6.1-Sol", "description": "Workhorse.",
        "default_reasoning_level": "low",
        "supported_reasoning_levels": [{"effort": "low"}, {"effort": "high"}, {"effort": "ultra"}],
        "service_tiers": [{"id": "priority", "name": "Fast", "description": "2x speed, increased usage"}],
        "base_instructions": "Be brief."
    }, {"slug": "gpt-plain"}]}))
    .unwrap();
    let ids: Vec<&str> = listed
        .iter()
        .map(|listed| listed.model.id.as_str())
        .collect();
    assert_eq!(ids, ["gpt-6.1-sol", "gpt-6.1-sol+fast", "gpt-plain"]);
    let (standard, fast) = (&listed[0].model, &listed[1].model);
    assert_eq!(standard.efforts, ["low", "high", "ultra"]);
    assert_eq!(standard.default_effort.as_deref(), Some("low"));
    assert_eq!(standard.description.as_deref(), Some("Workhorse."));
    assert_eq!(fast.name.as_deref(), Some("GPT-6.1-Sol Fast"));
    assert_eq!(fast.efforts, standard.efforts);
    assert_eq!(
        fast.description.as_deref(),
        Some("2x speed, increased usage")
    );
    assert_eq!(listed[1].instructions.as_deref(), Some("Be brief."));
    assert!(listed[2].model.efforts.is_empty() && !listed[2].model.reasoning);
}

#[test]
fn a_fast_id_asks_for_the_priority_tier() {
    let mut body = json!({"model": "gpt-6.1-sol+fast"});
    fast(body.as_object_mut().unwrap());
    assert_eq!(
        body,
        json!({"model": "gpt-6.1-sol", "service_tier": "priority"})
    );
    let mut body = json!({"model": "gpt-6.1-sol"});
    fast(body.as_object_mut().unwrap());
    assert_eq!(body, json!({"model": "gpt-6.1-sol"}));
    assert_eq!(model_of("+fast"), ("+fast", false));
}
