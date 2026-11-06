use super::*;

#[test]
fn only_the_official_opus_api_models_advertise_priority() {
    for (id, expected) in [
        ("claude-opus-4-8", true),
        ("claude-opus-5", true),
        ("claude-opus-5-5", true),
        ("CLAUDE-OPUS-5-5-20261001", true),
        ("claude-opus-4-8-20260501", true),
        ("claude-sonnet-5", false),
        ("claude-opus-4-7", false),
        ("claude-opus-5-5-2026100", false),
        ("claude-opus-5-5-latest", false),
    ] {
        for (subscription, api, base, official) in [
            (
                false,
                "anthropic-messages",
                "https://api.anthropic.com",
                true,
            ),
            (
                true,
                "anthropic-messages",
                "https://api.anthropic.com",
                false,
            ),
            (false, "anthropic-messages", "https://relay.example", false),
            (
                false,
                "bedrock-converse-stream",
                "https://api.anthropic.com",
                false,
            ),
        ] {
            let mut value = json!({"models":[{"id":id,"api":api,"baseUrl":base,"info":{"serviceTiers":["priority"]}}]});
            models(&mut value, subscription);
            assert_eq!(
                value["models"][0]["info"]["serviceTiers"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("priority")),
                expected && official,
                "{id} {subscription} {api} {base}"
            );
        }
    }
}

#[test]
fn priority_converts_to_speed_and_merges_beta_without_duplicates() {
    let target = Url::parse("https://api.anthropic.com/v1/messages").unwrap();
    let mut headers = BTreeMap::from([
        ("Anthropic-Beta".into(), format!("existing-beta, {BETA}")),
        ("anthropic-beta".into(), "second-beta".into()),
    ]);
    let mut body = Some(
        serde_json::to_vec(
            &json!({"model":"claude-opus-5-5-20261001","service_tier":"priority","messages":[]}),
        )
        .unwrap(),
    );
    finalize(&target, &mut headers, &mut body);
    let value: Value = serde_json::from_slice(body.as_ref().unwrap()).unwrap();
    assert_eq!(value["speed"], "fast");
    assert!(value.get("service_tier").is_none());
    let beta = &headers["anthropic-beta"];
    for expected in ["existing-beta", "second-beta", BETA] {
        assert_eq!(
            beta.split(',').filter(|value| *value == expected).count(),
            1
        );
    }
    assert_eq!(
        headers
            .keys()
            .filter(|name| name.eq_ignore_ascii_case("anthropic-beta"))
            .count(),
        1
    );
    let before = (headers.clone(), body.clone());
    finalize(&target, &mut headers, &mut body);
    assert_eq!((headers, body), before);
}

#[test]
fn relay_unknown_models_and_count_tokens_are_not_promoted_to_fast() {
    for (url, model) in [
        ("https://relay.example/v1/messages", "claude-opus-5-5"),
        ("https://api.anthropic.com/v1/messages", "claude-sonnet-5"),
        (
            "https://api.anthropic.com/v1/messages/count_tokens",
            "claude-opus-5-5",
        ),
    ] {
        let original = serde_json::to_vec(&json!({"model":model,"messages":[]})).unwrap();
        let mut body = Some(original.clone());
        let mut headers = BTreeMap::new();
        finalize(&Url::parse(url).unwrap(), &mut headers, &mut body);
        assert!(headers.is_empty());
        assert_eq!(body, Some(original));
    }
}

#[tokio::test]
async fn fast_is_applied_after_custom_headers_and_body_overrides() {
    use base64::Engine;
    let mut raw: Value = serde_json::from_str(include_str!("../nora-plugin.json")).unwrap();
    raw["contributes"]["provider"]["config"]["headers"] =
        json!({"Anthropic-Beta":"custom-fixture-beta","x-workspace":"private-workspace"});
    raw["contributes"]["provider"]["config"]["extraBody"] = json!({"!service_tier":"priority"});
    let mut definition = nora_provider::definition::Definition::parse(raw).unwrap();
    definition.adapter = std::sync::Arc::new(super::super::Anthropic);
    let request = nora_provider_plugin_sdk::FetchRequest {
        caller_user_agent: None,
        constraints: Default::default(),
        subscription_accounts_enabled: true,
        account: "fixture".into(),
        credentials: Some(json!({"type":"apiKey","access":"private-fixture-key"})),
        network_proxy: None,
        url: "https://api.anthropic.com/v1/messages".into(),
        method: "POST".into(),
        headers: BTreeMap::from([("anthropic-beta".into(), "caller-fixture-beta".into())]),
        metadata: Default::default(),
        body: Some(
            base64::engine::general_purpose::STANDARD
                .encode(br#"{"model":"claude-opus-5-5","service_tier":"auto","messages":[]}"#),
        ),
        body_file: None,
    };
    let built = nora_provider::transport::build_request(&definition, request)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(built.body.as_ref().unwrap()).unwrap();
    assert_eq!(body["speed"], "fast");
    assert!(body.get("service_tier").is_none());
    assert!(built.headers["anthropic-beta"]
        .split(',')
        .any(|value| value == BETA));
    assert!(built.headers["anthropic-beta"]
        .split(',')
        .any(|value| value == "custom-fixture-beta"));
    assert_eq!(built.headers["x-workspace"], "private-workspace");
    assert_eq!(built.headers["x-api-key"], "private-fixture-key");
}
