use super::*;

fn model(id: &str, api: &str, base: &str) -> Value {
    json!({"id":id,"api":api,"baseUrl":base,"info":{"serviceTiers":["auto","priority","ultrafast"]}})
}

#[test]
fn priority_is_limited_to_official_responses_chat_models() {
    let mut result = json!({"models":[
        model("gpt-6-sol","openai-responses","https://api.openai.com/v1"),
        model("o3","openai-responses","https://api.openai.com/v1"),
        model("gpt-6-sol","openai-completions","https://api.openai.com/v1"),
        model("gpt-6-sol","openai-responses","https://relay.example/v1"),
        model("text-embedding-3-large","openai-responses","https://api.openai.com/v1"),
        model("other","openai-responses","https://api.openai.com/v1")
    ]});
    models(&mut result, false);
    for (index, expected) in [true, true, false, false, false, false]
        .into_iter()
        .enumerate()
    {
        let tiers = result["models"][index]["info"]["serviceTiers"]
            .as_array()
            .unwrap();
        assert_eq!(tiers.contains(&json!("priority")), expected, "{index}");
        assert!(!tiers.contains(&json!("ultrafast")));
    }
}

#[test]
fn codex_priority_does_not_invent_ultrafast_or_include_media_models() {
    let mut result = json!({"models":[
        model("gpt-6-sol","chatgpt-codex","https://chatgpt.com"),
        {"id":"gpt-6-luna","api":"chatgpt-codex","baseUrl":"https://chatgpt.com"},
        {"id":"gpt-image-2","api":"chatgpt-codex","baseUrl":"https://chatgpt.com","info":{"chat":false,"imageOutput":true}},
        model("o3","chatgpt-codex","https://chatgpt.com"),
        model("gpt-6-sol","chatgpt-codex","https://relay.example")
    ]});
    models(&mut result, true);
    assert_eq!(
        result["models"][0]["info"]["serviceTiers"],
        json!(["auto", "priority", "ultrafast"])
    );
    assert_eq!(
        result["models"][1]["info"]["serviceTiers"],
        json!(["auto", "priority"])
    );
    for model in &result["models"].as_array().unwrap()[2..] {
        assert_eq!(model["info"]["serviceTiers"], json!([]));
    }
}

#[test]
fn codex_accepts_only_priority_and_explicit_ultrafast() {
    let url = Url::parse("https://chatgpt.com/backend-api/codex/responses").unwrap();
    for tier in [
        json!("auto"),
        json!("default"),
        json!("flex"),
        json!("fast"),
        Value::Null,
        json!(7),
        json!("priority"),
        json!("ultrafast"),
    ] {
        let original = json!({"model":"gpt-6-sol","service_tier":tier,"input":"hello"});
        let mut body = Some(serde_json::to_vec(&original).unwrap());
        finalize(true, &url, &mut body);
        let actual: Value = serde_json::from_slice(body.as_ref().unwrap()).unwrap();
        if matches!(tier.as_str(), Some("priority" | "ultrafast")) {
            assert_eq!(actual, original);
        } else {
            assert!(actual.get("service_tier").is_none());
            assert_eq!(actual["input"], "hello");
        }
    }
    for (subscription, target) in [
        (false, url),
        (
            true,
            Url::parse("https://chatgpt.com/backend-api/codex/images/generations").unwrap(),
        ),
    ] {
        let original = br#"{"service_tier":"flex","input":"keep"}"#.to_vec();
        let mut body = Some(original.clone());
        finalize(subscription, &target, &mut body);
        assert_eq!(body, Some(original));
    }
}

#[tokio::test]
async fn api_priority_is_kept_after_the_entire_request_builder() {
    use base64::Engine;
    let mut definition = nora_provider::definition::Definition::parse(
        serde_json::from_str(include_str!("../nora-provider.json")).unwrap(),
    )
    .unwrap();
    definition.adapter = std::sync::Arc::new(super::super::OpenAI);
    let body = json!({"model":"gpt-6-sol","input":"hello","service_tier":"priority"});
    let request = nora_provider_plugin_sdk::FetchRequest {
        subscription_accounts_enabled: true,
        account: "fixture".into(),
        credentials: Some(json!({"type":"apiKey","access":"private-fixture-key"})),
        network_proxy: None,
        url: "https://api.openai.com/v1/responses".into(),
        method: "POST".into(),
        headers: Default::default(),
        metadata: Default::default(),
        body: Some(
            base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&body).unwrap()),
        ),
        body_file: None,
    };
    let built = nora_provider::transport::build_request(&definition, request)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(built.body.as_ref().unwrap()).unwrap(),
        body
    );
    assert_eq!(built.headers["Authorization"], "Bearer private-fixture-key");
}

#[test]
fn ultrafast_must_be_offered_by_this_account_not_inherited_from_a_shared_catalog() {
    use nora_provider::ProviderAdapter;
    let provider = super::super::OpenAI;
    let credentials = json!({"type":"oauth","access":"private-fixture"});
    for (entry, expected) in [
        (json!({"slug":"gpt-6-sol"}), false),
        (json!({"service_tiers":[]}), false),
        (json!({"service_tiers":[{"id":"priority"}]}), false),
        (json!({"service_tiers":[{"id":"ultrafast"}]}), true),
        (json!({"service_tiers":["ultrafast"]}), true),
    ] {
        let mut model = nora_ai::Model::new("gpt-6-sol", "chatgpt-codex", "openai");
        model.info.service_tiers = vec!["auto".into(), "priority".into(), "ultrafast".into()];
        provider.enrich_model(&entry, &mut model, &credentials);
        assert_eq!(
            model
                .info
                .service_tiers
                .iter()
                .any(|tier| tier == "ultrafast"),
            expected
        );
    }
}
