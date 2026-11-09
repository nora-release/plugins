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
fn only_picked_chat_models_served_on_chat_completions_are_listed() {
    let listed = models(&json!({"data": [
        {"id": "gpt-4.1", "name": "GPT-4.1", "model_picker_enabled": true,
         "capabilities": {"type": "chat", "limits": {"max_context_window_tokens": 128000, "max_output_tokens": 16384},
                          "supports": {"vision": true}}},
        {"id": "claude-sonnet-4.5", "model_picker_enabled": true, "policy": {"state": "enabled"},
         "capabilities": {"type": "chat", "supports": {"max_thinking_budget": 32000}},
         "supported_endpoints": ["/chat/completions", "/v1/messages"]},
        {"id": "gpt-5-codex", "model_picker_enabled": true,
         "capabilities": {"type": "chat"}, "supported_endpoints": ["/responses"]},
        {"id": "grok-code", "model_picker_enabled": true, "policy": {"state": "unconfigured", "terms": "t"},
         "capabilities": {"type": "chat"}},
        {"id": "gpt-4o-2024-05-13", "model_picker_enabled": false, "capabilities": {"type": "chat"}},
        {"id": "text-embedding-3-small", "model_picker_enabled": true, "capabilities": {"type": "embeddings"}},
        {"id": "copilot-search-1", "model_picker_enabled": true, "capabilities": {"type": "chat"}}
    ]}));
    let ids: Vec<&str> = listed.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(ids, ["gpt-4.1", "claude-sonnet-4.5"]);
    assert_eq!(listed[0].name.as_deref(), Some("GPT-4.1"));
    assert_eq!(listed[0].context, Some(128_000));
    assert_eq!(listed[0].output, Some(16_384));
    assert!(listed[0].images && !listed[0].reasoning);
    assert!(listed[1].reasoning);
}

#[test]
fn a_turn_is_the_users_only_when_they_typed_it() {
    assert_eq!(
        initiator(&json!({"messages": [{"role": "user", "content": "hi"}]})),
        "user"
    );
    assert_eq!(
        initiator(&json!({"messages": [{"role": "user"}, {"role": "tool", "content": "42"}]})),
        "agent"
    );
    assert_eq!(initiator(&json!({})), "agent");
}

#[test]
fn an_image_part_asks_for_vision() {
    assert!(has_image(
        &json!({"messages": [{"role": "user", "content": [
            {"type": "text", "text": "what"}, {"type": "image_url", "image_url": {"url": "data:"}}
        ]}]})
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
