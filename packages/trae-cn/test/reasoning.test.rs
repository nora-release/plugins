use super::*;
use crate::tests::{answer, collect, configured, credentials, Client};
use base64::{engine::general_purpose::STANDARD, Engine};
use nora_ai::{Context, Model, OpenAICompletionsProvider, ReasoningLevel, StreamOptions};

fn config(options: Option<Value>) -> Value {
    let mut model = json!({
        "config_name":"model", "usage":"chat_completion",
        "display_config":{"display_name":"Live model"},
        "context_window_tokens":{"dev":200000},
        "model_detail_list":[{"model_name":"model__dev","max_tokens":32000}]
    });
    if let Some(options) = options {
        model["reasoning_effort_config"] = json!({"options":options,"default_level":"high"});
    }
    model
}

fn listing(configs: &[Value]) -> Value {
    json!({"function_configs":[{"function":"chat_v3","config_info_list":configs}]})
}

fn entry(options: Option<Value>) -> models::Entry {
    models::merge(
        Site::China,
        &BTreeMap::from([("chat_v3".into(), vec![config(options)])]),
    )
    .unwrap()
    .remove(0)
}

fn public_model(entry: &models::Entry, site: Site) -> Model {
    serde_json::from_value(entry.model(site, &credentials()).unwrap()).unwrap()
}

fn request(raw: Value, chat: Value, site: Site) -> FetchRequest {
    serde_json::from_value(json!({
        "account":"fixture-account","credentials":raw,"method":"POST",
        "url":format!("{}/v1/chat/completions",site.api(&json!({}))),
        "body":STANDARD.encode(chat.to_string())
    }))
    .unwrap()
}

fn chat() -> Value {
    json!({"model":"model","stream":false,"messages":[{"role":"user","content":"hi"}]})
}

fn sent_chat(requests: &[HttpRequest]) -> Vec<Value> {
    requests
        .iter()
        .filter(|request| request.url.ends_with("/api/agent/v3/llm_utils_chat"))
        .map(|request| serde_json::from_slice(request.body.as_ref().unwrap()).unwrap())
        .collect()
}

#[test]
fn live_options_are_exposed_in_the_order_of_the_ui_names_without_duplicates() {
    for (options, expected) in [
        (
            json!(["light", "high", "extra_high"]),
            vec!["low", "high", "xhigh"],
        ),
        (
            json!(["extra_high", "light", "high", "light"]),
            vec!["xhigh", "low", "high"],
        ),
        (json!(["ultra", "high", null, 12]), vec!["high"]),
        (json!(["low", "xhigh", "max"]), vec![]),
    ] {
        let model = public_model(&entry(Some(options)), Site::China);
        assert_eq!(model.info.reasoning_efforts, expected);
        assert!(model.reasoning);
    }
}

#[test]
fn no_known_levels_keeps_reasoning_enabled_but_does_not_invent_ui_options_or_defaults() {
    for options in [
        None,
        Some(json!([])),
        Some(json!(["ultra"])),
        Some(json!(null)),
        Some(json!("high")),
    ] {
        let entry = entry(options);
        let model = public_model(&entry, Site::China);
        assert!(model.reasoning);
        assert!(model.info.reasoning_efforts.is_empty());
        let mut input = chat();
        input["reasoning_effort"] = json!("high");
        assert!(body(&input, "chat_v3", &entry, "session")
            .unwrap()
            .get("reasoning_effort")
            .is_none());
    }
}

#[test]
fn all_public_and_native_spellings_only_send_levels_the_model_advertised() {
    let all = entry(Some(json!(["light", "high", "extra_high"])));
    for (asked, expected) in [
        ("low", Some("light")),
        ("high", Some("high")),
        ("xhigh", Some("extra_high")),
        ("light", Some("light")),
        ("extra_high", Some("extra_high")),
        ("none", None),
        ("minimal", None),
        ("medium", None),
        ("max", None),
        ("ultra", None),
        ("auto", None),
        ("", None),
        ("HIGH", None),
        (" high ", None),
    ] {
        let mut input = chat();
        input["reasoning_effort"] = json!(asked);
        let output = body(&input, "chat_v3", &all, "session").unwrap();
        assert_eq!(
            output.get("reasoning_effort").and_then(Value::as_str),
            expected,
            "requested {asked}"
        );
    }
    let high_only = entry(Some(json!(["high"])));
    for asked in ["low", "light", "xhigh", "extra_high"] {
        let mut input = chat();
        input["reasoning_effort"] = json!(asked);
        assert!(body(&input, "chat_v3", &high_only, "session")
            .unwrap()
            .get("reasoning_effort")
            .is_none());
    }
    assert!(body(&chat(), "chat_v3", &all, "session")
        .unwrap()
        .get("reasoning_effort")
        .is_none());
}

#[test]
fn request_field_aliases_follow_nullish_precedence_without_coercing_bad_values() {
    let entry = entry(Some(json!(["light", "high", "extra_high"])));
    for (snake, camel, expected) in [
        (None, Some(json!("xhigh")), Some("extra_high")),
        (Some(json!(null)), Some(json!("low")), Some("light")),
        (Some(json!("high")), Some(json!("low")), Some("high")),
        (Some(json!("none")), Some(json!("high")), None),
        (Some(json!("")), Some(json!("high")), None),
        (Some(json!(false)), Some(json!("high")), None),
        (Some(json!(1)), Some(json!("high")), None),
        (Some(json!({"effort":"high"})), None, None),
        (None, Some(json!(["high"])), None),
        (Some(json!(null)), Some(json!(null)), None),
    ] {
        let mut input = chat();
        if let Some(snake) = snake {
            input["reasoning_effort"] = snake;
        }
        if let Some(camel) = camel {
            input["reasoningEffort"] = camel;
        }
        let output = body(&input, "chat_v3", &entry, "session").unwrap();
        assert_eq!(
            output.get("reasoning_effort").and_then(Value::as_str),
            expected,
            "{input}"
        );
        assert!(output.get("reasoningEffort").is_none());
    }
}

#[test]
fn known_levels_break_equal_model_candidates_ties_and_choose_the_request_function() {
    let entries = models::merge(
        Site::China,
        &BTreeMap::from([
            ("chat_v3".into(), vec![config(None)]),
            (
                "solo_work_lite".into(),
                vec![config(Some(json!(["light", "high", "extra_high"])))],
            ),
        ]),
    )
    .unwrap();
    assert_eq!(entries[0].function, "solo_work_lite");
    assert_eq!(
        public_model(&entries[0], Site::China)
            .info
            .reasoning_efforts,
        ["low", "high", "xhigh"]
    );
}

#[test]
fn name_and_native_model_agreement_outrank_another_candidates_reasoning_levels() {
    let mut other = config(Some(json!(["light", "high"])));
    other["display_config"]["display_name"] = json!("Different model name");
    other["model_detail_list"] = json!([]);
    let entries = models::merge(
        Site::China,
        &BTreeMap::from([
            ("chat_v3".into(), vec![config(None)]),
            ("solo_work_lite".into(), vec![config(None)]),
            ("solo_agent".into(), vec![other]),
        ]),
    )
    .unwrap();
    assert_eq!(entries[0].function, "chat_v3");
    assert_eq!(entries[0].name, "Live model");
    assert!(public_model(&entries[0], Site::China)
        .info
        .reasoning_efforts
        .is_empty());
}

#[test]
fn max_uses_the_chosen_base_models_levels_even_if_its_function_has_different_levels() {
    let normal = config(Some(json!(["light"])));
    let mut larger = config(Some(json!(["high", "extra_high"])));
    larger["context_window_tokens"]["max"] = json!(1000000);
    larger["model_detail_list"]
        .as_array_mut()
        .unwrap()
        .push(json!({"model_name":"model__max","max_tokens":64000}));
    let entries = models::merge(
        Site::China,
        &BTreeMap::from([
            ("chat_v3".into(), vec![normal]),
            ("solo_work_lite".into(), vec![larger]),
        ]),
    )
    .unwrap();
    let normal = entries.iter().find(|entry| entry.id == "model").unwrap();
    let max = entries
        .iter()
        .find(|entry| entry.id == "model-max")
        .unwrap();
    assert_eq!(normal.function, "chat_v3");
    assert_eq!(max.function, "solo_work_lite");
    assert_eq!(
        public_model(normal, Site::China).info.reasoning_efforts,
        ["low"]
    );
    assert_eq!(
        public_model(max, Site::China).info.reasoning_efforts,
        ["low"]
    );
    for (asked, expected) in [("low", Some("light")), ("high", None)] {
        let mut input = chat();
        input["model"] = json!("model-max");
        input["reasoning_effort"] = json!(asked);
        let output = body(&input, "solo_work_lite", max, "session").unwrap();
        assert_eq!(output["config_name"], "model");
        assert_eq!(output["model_name"], "model__max");
        assert_eq!(
            output.get("reasoning_effort").and_then(Value::as_str),
            expected
        );
    }
}

#[tokio::test]
async fn every_ui_option_survives_the_model_catalog_and_nora_encoder_to_each_sites_http_request() {
    for site in [Site::China, Site::Global] {
        let client = Client::new(vec![
            (
                200,
                listing(&[config(Some(json!(["light", "high", "extra_high"])))]),
            ),
            (200, answer()),
            (200, answer()),
            (200, answer()),
        ]);
        let provider = configured(site, client.clone());
        let models = provider
            .models(CallContext {
                credentials: Some(credentials()),
                ..Default::default()
            })
            .await
            .unwrap();
        let model: Model = serde_json::from_value(models["models"][0].clone()).unwrap();
        assert_eq!(model.info.reasoning_efforts, ["low", "high", "xhigh"]);
        for (chosen, expected) in [
            (ReasoningLevel::Low, "light"),
            (ReasoningLevel::High, "high"),
            (ReasoningLevel::XHigh, "extra_high"),
        ] {
            let input = OpenAICompletionsProvider::default().encode_body(
                &model,
                &Context::default(),
                &StreamOptions {
                    reasoning: Some(chosen),
                    ..Default::default()
                },
            );
            let result = provider
                .fetch(request(credentials(), input, site))
                .await
                .unwrap();
            assert_eq!(result.status, 200);
            assert!(String::from_utf8(collect(result).await)
                .unwrap()
                .contains("hello"));
            let requests = client.requests.lock().unwrap();
            let sent = sent_chat(&requests);
            assert_eq!(sent.last().unwrap()["reasoning_effort"], expected);
            assert_eq!(
                requests.last().unwrap().url,
                format!("{}/api/agent/v3/llm_utils_chat", site.api(&credentials()))
            );
        }
        assert_eq!(
            client.requests.lock().unwrap().len(),
            4,
            "one cached list serves all three UI choices"
        );
    }
}

#[tokio::test]
async fn none_absent_and_unsupported_efforts_leave_the_vendor_default_in_real_requests() {
    let client = Client::new(vec![
        (
            200,
            listing(&[config(Some(json!(["light", "high", "extra_high"])))]),
        ),
        (200, answer()),
        (200, answer()),
        (200, answer()),
    ]);
    let provider = configured(Site::China, client.clone());
    for asked in [None, Some("none"), Some("medium")] {
        let mut input = chat();
        if let Some(asked) = asked {
            input["reasoning_effort"] = json!(asked);
        }
        let result = provider
            .fetch(request(credentials(), input, Site::China))
            .await
            .unwrap();
        assert_eq!(result.status, 200);
        collect(result).await;
    }
    let requests = client.requests.lock().unwrap();
    let sent = sent_chat(&requests);
    assert_eq!(sent.len(), 3);
    assert!(sent
        .iter()
        .all(|body| body.get("reasoning_effort").is_none()));
}

#[tokio::test]
async fn function_retries_keep_the_supported_effort_and_do_not_reset_the_conversation() {
    let client = Client::new(vec![
        (
            200,
            listing(&[config(Some(json!(["light", "high", "extra_high"])))]),
        ),
        (400, json!({"code":4001,"message":"wrong function"})),
        (200, answer()),
    ]);
    let provider = configured(Site::China, client.clone());
    let mut input = chat();
    input["reasoning_effort"] = json!("xhigh");
    let result = provider
        .fetch(request(credentials(), input, Site::China))
        .await
        .unwrap();
    assert_eq!(result.status, 200);
    collect(result).await;
    let requests = client.requests.lock().unwrap();
    let sent = sent_chat(&requests);
    assert_eq!(sent.len(), 2);
    assert_ne!(sent[0]["function"], sent[1]["function"]);
    assert_eq!(sent[0]["session_id"], sent[1]["session_id"]);
    assert!(sent
        .iter()
        .all(|body| body["reasoning_effort"] == "extra_high"));
}

#[tokio::test]
async fn refreshed_and_different_account_catalogs_never_reuse_another_levels() {
    let client = Client::new(vec![
        (200, listing(&[config(Some(json!(["extra_high"])))])),
        (200, answer()),
        (200, listing(&[config(Some(json!(["high"])))])),
        (200, answer()),
        (200, listing(&[config(None)])),
        (200, answer()),
    ]);
    let provider = configured(Site::China, client.clone());
    let first = credentials();
    let mut second = first.clone();
    second["uid"] = json!("another-member");
    second["accountId"] = json!("another@example.test");
    let mut input = chat();
    input["reasoning_effort"] = json!("xhigh");
    for account in [&first, &second] {
        let response = provider
            .fetch(request(account.clone(), input.clone(), Site::China))
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        collect(response).await;
    }
    provider
        .models(CallContext {
            credentials: Some(first.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    let response = provider
        .fetch(request(first, input, Site::China))
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    collect(response).await;
    let requests = client.requests.lock().unwrap();
    let sent = sent_chat(&requests);
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[0]["reasoning_effort"], "extra_high");
    assert!(sent[1].get("reasoning_effort").is_none());
    assert!(sent[2].get("reasoning_effort").is_none());
}
