use super::*;
use serde_json::json;

#[test]
fn a_session_names_its_api_and_expiry() {
    let read = session(&json!({
        "token": "tid=1", "expires_at": 1_900_000_000u64,
        "endpoints": {"api": "https://api.business.githubcopilot.com/"}
    }))
    .unwrap();
    assert_eq!(read.api, "https://api.business.githubcopilot.com");
    assert_eq!(
        read.expires,
        UNIX_EPOCH + Duration::from_secs(1_900_000_000)
    );
    assert!(read.fresh());
    assert_eq!(session(&json!({"token": "t"})).unwrap().api, API);
    assert!(session(&json!({"message": "no"})).is_none());
}

#[test]
fn a_session_about_to_expire_is_not_fresh() {
    let read = session(&json!({"token": "t", "expires_at": 1u64})).unwrap();
    assert!(!read.fresh());
}

#[test]
fn the_models_enabled_for_the_account_are_listed_with_their_apis() {
    let listed = models(&json!({"data": [
        {"id": "gpt-4.1", "name": "GPT-4.1", "model_picker_enabled": true,
         "capabilities": {"type": "chat", "limits": {"max_context_window_tokens": 128000, "max_output_tokens": 16384},
                          "supports": {"vision": true}}},
        {"id": "claude-sonnet-4.5", "model_picker_enabled": true, "policy": {"state": "enabled"},
         "capabilities": {"type": "chat", "supports": {"max_thinking_budget": 32000}},
         "supported_endpoints": ["/chat/completions", "/v1/messages"]},
        {"id": "gpt-5-codex", "model_picker_enabled": true,
         "capabilities": {"type": "chat"}, "supported_endpoints": ["/responses", "ws:/responses"]},
        {"id": "grok-code", "model_picker_enabled": true, "policy": {"state": "unconfigured", "terms": "t"},
         "capabilities": {"type": "chat"}},
        {"id": "gpt-4o-2024-05-13", "model_picker_enabled": false, "capabilities": {"type": "chat"}},
        {"id": "text-embedding-3-small", "model_picker_enabled": true, "capabilities": {"type": "embeddings"}},
        {"id": "copilot-search-1", "model_picker_enabled": true, "capabilities": {"type": "chat"}}
    ]}));
    let ids: Vec<&str> = listed.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(ids, ["gpt-4.1", "claude-sonnet-4.5", "gpt-5-codex"]);
    assert_eq!(listed[0].name.as_deref(), Some("GPT-4.1"));
    assert_eq!(listed[0].context, Some(128_000));
    assert_eq!(listed[0].output, Some(16_384));
    assert!(listed[0].images && !listed[0].reasoning);
    assert_eq!(listed[0].apis, [CHAT]);
    assert!(listed[1].reasoning);
    assert_eq!(listed[1].apis, [CHAT, MESSAGES]);
    assert_eq!(listed[2].apis, [RESPONSES]);
}

/// What a Free plan's `/models` answers: no model is the picker's, those
/// enabled for the account are in its categories.
#[test]
fn a_free_plan_lists_the_models_enabled_for_it() {
    let listed = models(&json!({"data": [
        {"id": "claude-opus-5", "vendor": "Anthropic", "model_picker_enabled": false, "model_picker_category": "powerful",
         "policy": {"state": "disabled"}, "capabilities": {"type": "chat"}, "supported_endpoints": ["/v1/messages", "/chat/completions"]},
        {"id": "gpt-6-luna", "vendor": "OpenAI", "model_picker_enabled": false, "model_picker_category": "lightweight",
         "policy": {"state": "enabled"}, "capabilities": {"type": "chat"}, "supported_endpoints": ["/responses", "ws:/responses"]},
        {"id": "kimi-k3-base", "vendor": "Experimental", "model_picker_enabled": false, "model_picker_category": "powerful",
         "policy": {"state": "enabled"}, "capabilities": {"type": "chat"}, "supported_endpoints": ["/chat/completions"]},
        {"id": "gpt-4.1-2025-04-14", "vendor": "Azure OpenAI", "model_picker_enabled": false,
         "policy": {"state": "enabled"}, "capabilities": {"type": "chat"}},
        {"id": "gpt-4.1", "vendor": "Azure OpenAI", "model_picker_enabled": false, "model_picker_category": "versatile",
         "policy": {"state": "enabled"}, "capabilities": {"type": "chat"}},
        {"id": "gpt-5.4-nano", "vendor": "OpenAI", "model_picker_enabled": false, "model_picker_category": "versatile",
         "capabilities": {"type": "chat"}, "supported_endpoints": ["/responses"]},
        {"id": "gpt-5.6-luna-free-auto", "vendor": "OpenAI", "model_picker_enabled": false, "model_picker_category": "lightweight",
         "capabilities": {"type": "chat"}, "supported_endpoints": ["/responses"]}
    ]}));
    let ids: Vec<&str> = listed.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(ids, ["gpt-6-luna", "gpt-4.1"]);
}

#[test]
fn a_request_goes_to_its_apis_path_at_copilot() {
    assert_eq!(
        route("/v1/chat/completions"),
        Some((CHAT, "/chat/completions"))
    );
    assert_eq!(
        route("/chat/completions"),
        Some((CHAT, "/chat/completions"))
    );
    assert_eq!(route("/v1/responses"), Some((RESPONSES, "/responses")));
    assert_eq!(route("/v1/messages"), Some((MESSAGES, "/v1/messages")));
    assert_eq!(route("/models"), None);
}

#[test]
fn a_turn_is_the_users_only_when_they_typed_it() {
    assert_eq!(
        initiator(
            CHAT,
            &json!({"messages": [{"role": "user", "content": "hi"}]})
        ),
        "user"
    );
    assert_eq!(
        initiator(
            CHAT,
            &json!({"messages": [{"role": "user"}, {"role": "tool", "content": "42"}]})
        ),
        "agent"
    );
    assert_eq!(initiator(CHAT, &json!({})), "agent");
    assert_eq!(initiator(RESPONSES, &json!({"input": "hi"})), "user");
    assert_eq!(
        initiator(
            RESPONSES,
            &json!({"input": [{"role": "user", "content": "hi"}, {"type": "function_call_output", "output": "42"}]})
        ),
        "agent"
    );
    assert_eq!(
        initiator(
            MESSAGES,
            &json!({"messages": [{"role": "user", "content": "hi"}]})
        ),
        "user"
    );
    assert_eq!(
        initiator(
            MESSAGES,
            &json!({"messages": [{"role": "user", "content": [{"type": "tool_result", "content": "42"}]}]})
        ),
        "agent"
    );
}

#[test]
fn an_image_asks_for_vision_in_any_api() {
    assert!(has_image(
        &json!({"messages": [{"role": "user", "content": [
            {"type": "text", "text": "what"}, {"type": "image_url", "image_url": {"url": "data:"}}
        ]}]})
    ));
    assert!(has_image(
        &json!({"input": [{"role": "user", "content": [{"type": "input_image", "image_url": "data:"}]}]})
    ));
    assert!(has_image(
        &json!({"messages": [{"role": "user", "content": [{"type": "image", "source": {}}]}]})
    ));
    assert!(!has_image(
        &json!({"messages": [{"role": "user", "content": "plain"}]})
    ));
}

#[test]
fn usage_reads_the_plan_and_each_limited_allowance() {
    let read = usage(&json!({
        "copilot_plan": "individual",
        "quota_reset_date": "2026-12-01",
        "quota_snapshots": {
            "chat": {"unlimited": true},
            "completions": {"unlimited": true},
            "premium_interactions": {"entitlement": "300", "quota_remaining": 225, "percent_remaining": 75.0, "unlimited": false}
        }
    }));
    assert_eq!(read.plan.as_deref(), Some("individual"));
    assert_eq!(read.windows.len(), 1);
    let premium = &read.windows[0];
    assert_eq!(premium.name, "Premium requests");
    assert_eq!(premium.used, Some(25.0));
    assert_eq!(premium.balance, Some(225.0));
    assert_eq!(premium.unit.as_deref(), Some("requests"));
    assert_eq!(premium.resets_at.as_deref(), Some("2026-12-01T00:00:00Z"));
}

#[test]
fn usage_works_out_the_share_used_without_a_percentage() {
    let read =
        usage(&json!({"quota_snapshots": {"chat": {"entitlement": 50, "quota_remaining": "40"}}}));
    assert_eq!(read.windows[0].used, Some(20.0));
}
