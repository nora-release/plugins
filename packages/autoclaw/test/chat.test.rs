use super::*;
use crate::tests::{collect, configured, credentials, Client};
use base64::{engine::general_purpose::STANDARD, Engine};
fn request(site: Site, body: Value) -> FetchRequest {
    FetchRequest {
        caller_user_agent: None,
        constraints: Default::default(),
        subscription_accounts_enabled: true,
        account: "one".into(),
        credentials: Some(credentials(site)),
        network_proxy: None,
        url: "https://untrusted.invalid/chat/completions".into(),
        method: "POST".into(),
        headers: BTreeMap::from([
            ("Authorization".into(), "Bearer caller-key".into()),
            ("X-Harness-Type".into(), "external".into()),
        ]),
        metadata: BTreeMap::new(),
        body: Some(STANDARD.encode(body.to_string())),
        body_file: None,
    }
}
#[test]
fn all_upstream_identity_replacements_preserve_user_and_tool_messages() {
    for (from, to) in [
        (
            "You are a Claude agent, built on Anthropic's Claude Agent SDK.",
            "You are a coding assistant.",
        ),
        ("the Claude Agent SDK", "the agent framework"),
        ("Anthropic's Claude Agent SDK", "the agent framework"),
        (
            "You are an AI agent powered by DeepSeek Harness",
            "You are an AI agent powered by DeepSeek-Harness",
        ),
        ("You are Claude Code", "You are the Claude Code CLI"),
        ("You are Codex", "You are the Codex agent"),
        ("Codex CLI", "Codex command-line tool"),
        ("You are opencode", "You are the opencode agent"),
        ("You are Cursor", "You are the Cursor agent"),
        ("You are Cline", "You are the Cline agent"),
        ("You are Qwen Code", "You are the Qwen Code agent"),
        ("You are ZCode", "You are the ZCode agent"),
        (
            "operating inside pi, a coding agent harness",
            "operating inside the pi coding agent harness",
        ),
    ] {
        let body=prepare(json!({"messages":[{"role":"developer","content":[{"type":"text","text":from}]},{"role":"user","content":from},{"role":"tool","content":from}]}),Site::International).unwrap();
        assert_eq!(body["messages"][1]["content"][0]["text"], to);
        assert_eq!(body["messages"][2]["content"], from);
        assert_eq!(body["messages"][3]["content"], from);
    }
}
#[test]
fn string_and_array_systems_keep_their_content_and_prefix_is_idempotent() {
    for site in [Site::China, Site::International] {
        for content in [
            json!("Custom instructions"),
            json!([{"type":"text","text":"Custom instructions"}]),
        ] {
            let first=prepare(json!({"messages":[{"role":"system","content":content},{"role":"user","content":"hi"}],"max_tokens":100,"max_completion_tokens":200,"stream":false}),site).unwrap();
            assert!(first.get("max_tokens").is_none());
            assert!(first.get("max_completion_tokens").is_none());
            assert!(first["messages"][0]["content"]
                .to_string()
                .contains("Custom instructions"));
            assert_eq!(first["stream"], false);
            assert_eq!(prepare(first.clone(), site).unwrap(), first);
        }
    }
}
#[tokio::test]
async fn global_chat_uses_the_fixed_gateway_correct_authentication_and_retries_the_other_side_on_401(
) {
    let client = Client::new(vec![
        (401, json!({"error":"expired on wrong side"})),
        (200, json!({"raw":"data: [DONE]\n\n"})),
    ]);
    let provider = configured(Site::International, client.clone());
    let response=provider.fetch(request(Site::International,json!({"model":"zai_auto","messages":[{"role":"user","content":"hi"}],"max_tokens":100}))).await.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(
        String::from_utf8(collect(response).await).unwrap(),
        "data: [DONE]\n\n"
    );
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].url.starts_with(Site::International.host()));
    assert!(requests[1].url.starts_with(Site::China.host()));
    assert!(requests[0]
        .url
        .ends_with("/autoclaw-proxy/proxy/autoclaw/v1/chat/completions"));
    assert_eq!(requests[0].headers["x-authorization"], "Bearer access-one");
    assert!(!requests[0].headers.contains_key("authorization"));
    assert!(!requests[0].headers.contains_key("x-harness-type"));
    assert_eq!(
        requests[0].headers["x-request-id"],
        requests[1].headers["x-request-id"]
    );
    let body: Value = serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(body["messages"][0]["content"], PREFIX);
    assert!(body.get("max_tokens").is_none());
}
#[tokio::test]
async fn china_uses_the_managed_gateway_with_current_identity_and_streams_both_modes() {
    let client = Client::new(vec![
        (200, json!({"choices":[]})),
        (200, json!({"raw":"data: [DONE]\n\n"})),
    ]);
    let provider = configured(Site::China, client.clone());
    for stream in [false, true] {
        let result=provider.fetch(request(Site::China,json!({"model":"zaicoding_glm-5.3","messages":[{"role":"system","content":"You are Codex. Custom instructions"}],"max_completion_tokens":123,"stream":stream}))).await.unwrap();
        assert_eq!(result.status, 200);
        collect(result).await;
    }
    let requests = client.requests.lock().unwrap();
    for request in requests.iter() {
        assert_eq!(request.url,"https://autoglm-acceleration-api.zhipuai.cn/autoclaw-proxy/proxy/autoclaw/chat/completions");
        assert_eq!(request.headers["x-version"], "2.1.0");
        assert_eq!(request.headers["x-channel"], "AutoClaw26");
        assert_eq!(request.headers["x-agent-id"], "main");
        assert_eq!(request.headers["x-request-model"], "zaicoding_glm-5.3");
        assert_eq!(request.headers["x-authorization"], "Bearer access-one");
        assert!(!request.headers.contains_key("authorization"));
    }
}
#[tokio::test]
async fn spent_403_is_quota_keeps_login_redacts_secrets_and_never_falls_back_to_another_host() {
    for site in [Site::International, Site::China] {
        for value in [
            json!({"code":810000,"message":"GLM 免费额度已用完 access-one"}),
            json!({"error":{"message":"quota exhausted"}}),
        ] {
            let client = Client::new(vec![(403, value)]);
            let provider = configured(site, client.clone());
            let result = provider
                .fetch(request(site, json!({"model":"zai_auto","messages":[]})))
                .await
                .unwrap();
            assert_eq!(result.status, 429);
            assert_eq!(result.headers["x-magpie-sign-in"], "kept");
            assert!(!String::from_utf8(collect(result).await)
                .unwrap()
                .contains("access-one"));
            assert_eq!(client.requests.lock().unwrap().len(), 1);
        }
    }
}
#[tokio::test]
async fn authentication_and_406_have_distinct_signin_and_recovery_hints() {
    for site in [Site::China, Site::International] {
        let client = Client::new(vec![(401, json!({})), (401, json!({}))]);
        let provider = configured(site, client.clone());
        let response = provider
            .fetch(request(site, json!({"model":"zai_auto","messages":[]})))
            .await
            .unwrap();
        assert_eq!(response.status, 401);
        assert_eq!(response.headers["x-magpie-sign-in"], "expired");
        assert_eq!(
            client.requests.lock().unwrap().len(),
            if site == Site::China { 1 } else { 2 }
        );
        let client = Client::new(vec![(406, json!({"raw":""}))]);
        let provider = configured(site, client.clone());
        let response = provider
            .fetch(request(site, json!({"model":"zai_auto","messages":[]})))
            .await
            .unwrap();
        assert_eq!(response.status, 406);
        assert_eq!(response.headers["x-magpie-sign-in"], "kept");
        assert!(String::from_utf8(collect(response).await)
            .unwrap()
            .contains("Choose another provider"));
        assert_eq!(client.requests.lock().unwrap().len(), 1);
    }
}
