use super::*;

fn configured(
    id: &'static str,
    option: Option<Value>,
    replies: Vec<(u16, Value)>,
) -> (Cline, Arc<Client>) {
    let (_, client, store) = setup(replies);
    let mut manifest: Value = serde_json::from_str(if id == "cline" {
        include_str!("../nora-provider.json")
    } else {
        include_str!("../../clinepass/nora-provider.json")
    })
    .unwrap();
    if let Some(option) = option {
        manifest["config"]["pinUpstream"] = option;
    }
    let core = Core::with_dependencies(id, client.clone(), store);
    core.initialize(InitializeParams {
        subscription_accounts_enabled: true,
        installed_manifest: None,
        runtime_config: Value::Null,
        api_version: 1,
        plugin_id: id.into(),
        plugin_directory: PathBuf::new(),
        data_directory: PathBuf::new(),
        manifest,
    })
    .unwrap();
    (Cline::with_core(core), client)
}

fn request(body: &[u8]) -> FetchRequest {
    serde_json::from_value(json!({"account":"fixture","credentials":{"type":"api","key":"fixture-key"},"method":"POST","url":format!("{BASE}/chat/completions"),
        "headers":{"Authorization":"Bearer wrong-caller","Accept":"text/event-stream","X-Task-ID":"wrong-task"},"metadata":{"sessionID":"private-session"},
        "body":base64::engine::general_purpose::STANDARD.encode(body)})).unwrap()
}

async fn fetch(plugin: &Cline, body: &str) -> FetchResponse {
    plugin.fetch(request(body.as_bytes())).await.unwrap()
}

fn body(model: &str) -> String {
    format!("{{ \"messages\" : [] , \"model\" : \"{model}\" }}\n")
}

fn pin(images: bool) -> Value {
    json!({"gateway":{"only":["deepseek"],"has":if images {vec!["vision","implicit-caching"]} else {vec!["implicit-caching"]}}})
}

fn sent(client: &Client) -> Vec<Value> {
    client
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| serde_json::from_slice(request.body.as_ref().unwrap()).unwrap())
        .collect()
}

#[test]
fn shipped_manifests_preserve_an_unspecified_pin_preference() {
    for text in [
        include_str!("../nora-provider.json"),
        include_str!("../../clinepass/nora-provider.json"),
    ] {
        let manifest: Value = serde_json::from_str(text).unwrap();
        assert!(manifest["config"].get("pinUpstream").is_none());
    }
}

#[tokio::test]
async fn default_pass_pin_uses_preset_image_capabilities_and_preserves_the_cache_prefix() {
    for id in ["cline", "clinepass"] {
        let (plugin, client) = configured(id, None, vec![(200, json!({})); 4]);
        for model in [
            "cline-pass/deepseek-v4.1-flash-1017",
            "cline-pass/deepseek-v4.1-flash",
            "cline-pass/deepseek-v4-pro",
            "cline-pass/deepseek-unknown",
        ] {
            let original = body(model);
            assert_eq!(fetch(&plugin, &original).await.status, 200);
            let requests = client.requests.lock().unwrap();
            let sent = requests.last().unwrap().body.as_ref().unwrap();
            assert!(sent.starts_with(&original.as_bytes()[..original.rfind('}').unwrap()]));
            assert!(sent.ends_with(b"}\n"));
        }
        assert_eq!(
            sent(&client)
                .iter()
                .map(|body| body["providerOptions"].clone())
                .collect::<Vec<_>>(),
            vec![pin(true), pin(true), pin(false), pin(false)]
        );
    }
}

#[tokio::test]
async fn image_parts_add_vision_even_for_unknown_models_without_mistaking_text_for_images() {
    let (plugin, client) = configured("clinepass", None, vec![(200, json!({})); 4]);
    for kind in ["image_url", "image", "input_image"] {
        let body = json!({"model":"cline-pass/deepseek-next","messages":[{"role":"user","content":[{"type":kind,"image_url":{"url":"data:image/png;base64,AA"}}]}]}).to_string();
        fetch(&plugin, &body).await;
    }
    fetch(&plugin, &json!({"model":"cline-pass/deepseek-next","messages":[{"role":"user","content":"type image_url; \"type\": \"image\""}]}).to_string()).await;
    assert_eq!(
        sent(&client)
            .iter()
            .map(|body| body["providerOptions"].clone())
            .collect::<Vec<_>>(),
        vec![pin(true), pin(true), pin(true), pin(false)]
    );
}

#[tokio::test]
async fn live_catalog_updates_image_requirements_without_an_extra_fetch_per_chat() {
    let (plugin, client) = configured(
        "clinepass",
        None,
        vec![
            (
                200,
                json!({"clinePass":["cline-pass/deepseek-v4.1-flash","cline-pass/deepseek-v4-pro"]}),
            ),
            (
                200,
                json!({"data":[{"id":"deepseek/deepseek-v4.1-flash","architecture":{"input_modalities":["text"]}},{"id":"deepseek/deepseek-v4-pro","architecture":{"input_modalities":["text","image"]}}]}),
            ),
            (200, json!({})),
            (200, json!({})),
        ],
    );
    plugin.models(CallContext::default()).await.unwrap();
    fetch(&plugin, &body("cline-pass/deepseek-v4.1-flash")).await;
    fetch(&plugin, &body("cline-pass/deepseek-v4-pro")).await;
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    for (index, expected) in [(2, pin(false)), (3, pin(true))] {
        let value: Value = serde_json::from_slice(requests[index].body.as_ref().unwrap()).unwrap();
        assert_eq!(value["providerOptions"], expected);
    }
}

#[tokio::test]
async fn explicit_pin_preferences_keep_false_unpinned_and_true_without_fallback() {
    for option in [json!(false), json!("false"), json!(true), json!("true")] {
        let (plugin, client) = configured(
            "clinepass",
            Some(option.clone()),
            vec![
                (503, json!({"error":"no matching upstream"})),
                (200, json!({})),
            ],
        );
        let original = body("cline-pass/deepseek-v4.1-flash");
        assert_eq!(fetch(&plugin, &original).await.status, 503);
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        if option == true || option == "true" {
            assert_eq!(
                serde_json::from_slice::<Value>(requests[0].body.as_ref().unwrap()).unwrap()
                    ["providerOptions"],
                pin(true)
            );
        } else {
            assert_eq!(requests[0].body.as_ref().unwrap(), original.as_bytes());
        }
    }
}

#[tokio::test]
async fn caller_routing_is_preserved_byte_for_byte_in_every_preference() {
    for option in [None, Some(json!(false)), Some(json!(true))] {
        for (key, value) in [
            ("providerOptions", json!({"gateway":{"only":["mine"]}})),
            ("provider", json!({"order":["novita"]})),
            ("providerOptions", Value::Null),
            ("provider", json!(false)),
        ] {
            let (plugin, client) = configured(
                "clinepass",
                option.clone(),
                vec![
                    (400, json!({"error":"no matching upstream"})),
                    (200, json!({})),
                ],
            );
            let original=format!("{{ \"model\":\"cline-pass/deepseek-v4.1-flash\", \"messages\":[], \"{key}\":{value} }}");
            assert_eq!(fetch(&plugin, &original).await.status, 400);
            let requests = client.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].body.as_ref().unwrap(), original.as_bytes());
        }
    }
}

#[tokio::test]
async fn default_leaves_other_models_unchanged_and_explicit_on_keeps_the_official_makers() {
    for model in [
        "cline-pass/kimi-k3",
        "cline-pass/glm-5.3",
        "cline-pass/glm-5.3-flash",
        "cline-pass/mimo-v2.6-pro",
        "cline-free/deepseek-v4.1-flash",
        "deepseek/deepseek-v4-pro",
        "anthropic/claude-sonnet-5.5",
    ] {
        let (plugin, client) = configured(
            "cline",
            None,
            vec![
                (400, json!({"error":"no matching upstream"})),
                (200, json!({})),
            ],
        );
        let original = body(model);
        assert_eq!(fetch(&plugin, &original).await.status, 400);
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].body.as_ref().unwrap(), original.as_bytes());
    }
    for (model, key, value) in [
        (
            "deepseek/deepseek-v4-pro",
            "providerOptions",
            json!({"gateway":{"only":["deepseek"]}}),
        ),
        (
            "cline-pass/glm-5.3",
            "providerOptions",
            json!({"gateway":{"only":["zai"]}}),
        ),
        (
            "cline-pass/glm-5.3-flash",
            "provider",
            json!({"only":["z-ai"]}),
        ),
    ] {
        let (plugin, client) = configured("cline", Some(json!(true)), vec![(200, json!({}))]);
        fetch(&plugin, &body(model)).await;
        assert_eq!(sent(&client)[0][key], value);
    }
}

#[tokio::test]
async fn automatic_pin_retries_once_as_original_with_the_same_account_and_session_headers() {
    for (status, error) in [
        (400, json!({"error":"no matching upstream"})),
        (
            404,
            json!({"error":{"message":"No available providers for this model"}}),
        ),
        (503, json!({"error":{"message":"upstream unavailable"}})),
        (500, json!({"_raw":"bad gateway"})),
    ] {
        for oauth in [false, true] {
            let (plugin, client) = configured(
                if oauth { "cline" } else { "clinepass" },
                None,
                vec![
                    (status, error.clone()),
                    (
                        200,
                        json!({"_raw":"data: {}\n\ndata: [DONE]\n\n","_content_type":"text/event-stream"}),
                    ),
                ],
            );
            let original = body("cline-pass/deepseek-v4.1-flash");
            let mut req = request(original.as_bytes());
            if oauth {
                req.credentials = Some(credentials());
            }
            let response = plugin.fetch(req).await.unwrap();
            assert_eq!(response.status, 200);
            assert_eq!(response.headers["content-type"], "text/event-stream");
            let bytes: Vec<_> = response
                .body
                .collect::<Vec<_>>()
                .await
                .into_iter()
                .flat_map(Result::unwrap)
                .collect();
            assert_eq!(bytes, b"data: {}\n\ndata: [DONE]\n\n");
            let requests = client.requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0].url, requests[1].url);
            assert_eq!(requests[0].headers, requests[1].headers);
            assert_eq!(
                requests[1].headers["authorization"],
                if oauth {
                    "Bearer workos:old-access"
                } else {
                    "Bearer fixture-key"
                }
            );
            assert_eq!(
                requests[1].headers["x-task-id"],
                headers(Some("private-session"))["x-task-id"]
            );
            assert_eq!(requests[1].body.as_ref().unwrap(), original.as_bytes());
            assert_eq!(
                serde_json::from_slice::<Value>(requests[0].body.as_ref().unwrap()).unwrap()
                    ["providerOptions"],
                pin(true)
            );
        }
    }
}

#[tokio::test]
async fn failed_retry_is_returned_and_later_requests_still_use_the_default_pin() {
    let (plugin, client) = configured(
        "clinepass",
        None,
        vec![
            (400, json!({"error":"no matching upstream"})),
            (503, json!({"error":{"message":"down"}})),
            (200, json!({})),
        ],
    );
    let original = body("cline-pass/deepseek-v4-pro");
    assert_eq!(fetch(&plugin, &original).await.status, 503);
    assert_eq!(client.requests.lock().unwrap().len(), 2);
    assert_eq!(fetch(&plugin, &original).await.status, 200);
    let sent = sent(&client);
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[0]["providerOptions"], pin(false));
    assert!(sent[1].get("providerOptions").is_none());
    assert_eq!(sent[2]["providerOptions"], pin(false));
}

#[tokio::test]
async fn account_refusals_and_invalid_requests_are_not_retried() {
    for (status, error) in [
        (401, "Unauthorized"),
        (402, "insufficient credits"),
        (403, "Not subscribed"),
        (429, "rate limit exceeded"),
        (400, "messages: field required"),
    ] {
        let (plugin, client) = configured(
            "clinepass",
            None,
            vec![
                (status, json!({"error":{"message":error}})),
                (200, json!({})),
            ],
        );
        let response = fetch(&plugin, &body("cline-pass/deepseek-v4.1-flash")).await;
        assert_eq!(response.status, if status == 402 { 429 } else { status });
        assert_eq!(client.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn a_transport_failure_is_not_an_http_pin_refusal() {
    let (plugin, client) = configured(
        "clinepass",
        None,
        vec![(0, json!({"_transport_error":true})), (200, json!({}))],
    );
    assert!(plugin
        .fetch(request(body("cline-pass/deepseek-v4.1-flash").as_bytes()))
        .await
        .is_err());
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn unspecified_like_values_keep_automatic_routing_and_explicit_on_never_pins_free_models() {
    for option in [Value::Null, json!(""), json!(0), json!({})] {
        let (plugin, client) = configured("clinepass", Some(option), vec![(200, json!({}))]);
        fetch(&plugin, &body("cline-pass/deepseek-v4-pro")).await;
        assert_eq!(sent(&client)[0]["providerOptions"], pin(false));
    }
    let (plugin, client) = configured(
        "cline",
        Some(json!(true)),
        vec![(503, json!({"error":"down"}))],
    );
    let original = body("cline-free/deepseek-v4.1-flash");
    assert_eq!(fetch(&plugin, &original).await.status, 503);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].body.as_ref().unwrap(), original.as_bytes());
}

#[tokio::test]
async fn absent_ambiguous_and_failed_catalogs_keep_last_known_capabilities() {
    let rec = json!({"clinePass":["cline-pass/deepseek-v4.1-flash","cline-pass/deepseek-v4-pro"]});
    let known = json!([
        {"id":"deepseek/deepseek-v4.1-flash","input_modalities":["text"]},
        {"id":"deepseek/deepseek-v4-pro","input_modalities":["text","image"]}
    ]);
    for cloud in [
        Value::Null,
        json!([
            {"id":"cline-pass/deepseek-v4.1-flash"},
            {"id":"deepseek/deepseek-v4.1-flash","input_modalities":["text","image"]},
            {"id":"cline-pass/deepseek-v4-pro","input_modalities":[]},
            {"id":"deepseek/deepseek-v4-pro","input_modalities":["text"]}
        ]),
        json!([
            {"id":"deepseek/deepseek-v4.1-flash","input_modalities":["text","image"]},
            {"id":"other/deepseek-v4.1-flash","input_modalities":["text","image","audio"]},
            {"id":"deepseek/deepseek-v4-pro","input_modalities":["text"]},
            {"id":"other/deepseek-v4-pro","input_modalities":["text","image"]}
        ]),
        json!({"_fail":true}),
    ] {
        let mut replies = vec![(200, rec.clone()), (200, known.clone())];
        let failed = cloud["_fail"] == true;
        if failed {
            replies.push((503, json!({"error":"unavailable"})));
        } else {
            replies.extend([(200, rec.clone()), (200, cloud)]);
        }
        replies.extend([(200, json!({})), (200, json!({}))]);
        let (plugin, client) = configured("clinepass", None, replies);
        plugin.models(CallContext::default()).await.unwrap();
        let context = CallContext {
            credentials: Some(json!({"type":"api","key":"fixture"})),
            ..CallContext::default()
        };
        assert_eq!(plugin.models(context).await.is_err(), failed);
        fetch(&plugin, &body("cline-pass/deepseek-v4.1-flash")).await;
        fetch(&plugin, &body("cline-pass/deepseek-v4-pro")).await;
        let requests = client.requests.lock().unwrap();
        for (request, expected) in requests
            .iter()
            .rev()
            .take(2)
            .rev()
            .zip([pin(false), pin(true)])
        {
            let value: Value = serde_json::from_slice(request.body.as_ref().unwrap()).unwrap();
            assert_eq!(value["providerOptions"], expected);
        }
    }
}

#[tokio::test]
async fn image_parts_override_a_text_only_catalog_entry_and_keep_original_bytes() {
    let (plugin, client) = configured("clinepass", None, vec![(200, json!({}))]);
    let original="{\n \"messages\": [{\"role\":\"user\",\"content\":[{\"type\" : \"input_image\",\"url\":\"https://example.test/a\"},{\"type\":\"text\",\"text\":\"中文 \\\"quoted\\\" \\u4f60\"}]}], \"model\":\"cline-pass/deepseek-v4-pro\"  } \n";
    fetch(&plugin, original).await;
    let requests = client.requests.lock().unwrap();
    let bytes = requests[0].body.as_ref().unwrap();
    assert!(bytes.starts_with(&original.as_bytes()[..original.rfind('}').unwrap()]));
    assert!(bytes.ends_with(b"} \n"));
    assert_eq!(
        serde_json::from_slice::<Value>(bytes).unwrap()["providerOptions"],
        pin(true)
    );
}

#[tokio::test]
async fn retry_transport_failure_is_returned_without_a_third_attempt() {
    let (plugin, client) = configured(
        "clinepass",
        None,
        vec![
            (503, json!({"error":"down"})),
            (0, json!({"_transport_error":true})),
            (200, json!({})),
        ],
    );
    let error = plugin
        .fetch(request(body("cline-pass/deepseek-v4-pro").as_bytes()))
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, "upstream_transport_error");
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn large_error_pages_use_the_official_bounded_prefix_for_retry_decisions() {
    let padding = "x".repeat(1024 * 1024 + 10);
    for (status, text, expected) in [
        (503, padding.clone(), 200),
        (400, format!("No eligible endpoints: {padding}"), 200),
        (400, format!("{padding} no matching upstream"), 400),
    ] {
        let (plugin, client) = configured(
            "clinepass",
            None,
            vec![(status, json!({"_raw":text})), (200, json!({}))],
        );
        assert_eq!(
            fetch(&plugin, &body("cline-pass/deepseek-v4-pro"))
                .await
                .status,
            expected
        );
        assert_eq!(
            client.requests.lock().unwrap().len(),
            if expected == 200 { 2 } else { 1 }
        );
    }
}
