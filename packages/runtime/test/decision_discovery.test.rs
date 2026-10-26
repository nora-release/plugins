use super::*;

fn mixed_definition(kind: &str, base: &str) -> Definition {
    Definition::parse(json!({
        "id":"fixture-mixed", "name":"Mixed", "api":"openai-completions", "baseURL":base,
        "config":{"keyProvider":"fixture-mixed", "decisionDiscovery":kind, "apiKeyAuth":"bearer"}
    }))
    .unwrap()
}

fn chat_list() -> Reply {
    Reply::json(json!({"data":[{"id":"router"},{"id":"jev-chat"},{"id":"ordinary"}]}))
}

async fn mixed_fetch(definition: &Definition, replies: Vec<Reply>) -> (RemoteModels, MockClient) {
    let client = client(replies);
    let found = fetch_routes(
        definition,
        &api_credentials(),
        &resolved(),
        &catalog(),
        &client,
    )
    .await
    .unwrap();
    (found, client)
}

fn assert_decision(model: &Model, context: u64, image: bool) {
    assert_eq!(model.api, "systemone");
    assert_eq!(model.info.decision, Some(true));
    assert_eq!(model.info.chat, Some(false));
    assert_eq!(model.info.tool_calling, Some(false));
    assert_eq!(model.context_window, context);
    assert_eq!(model.input.contains(&nora_ai::InputModality::Image), image);
    assert!(!model.provider_options.contains_key("decision"));
}

#[tokio::test]
async fn mixed_omlx_uses_status_aliases_and_server_types_instead_of_model_names() {
    let definition = mixed_definition("omlx", "https://local.invalid/prefix/v1");
    let (found, client) = mixed_fetch(&definition, vec![chat_list(), Reply::json(json!({"models":[
        {"id":"folder", "model_alias":"router", "model_type":"decision", "max_context_window":2048, "model_context_length":4096},
        {"id":"second", "engine_type":"decision", "model_context_length":8192},
        {"id":"hidden", "model_type":"decision", "is_hidden":true},
        {"id":"jev-chat", "model_type":"llm"}
    ]}))]).await;
    let router = found.models.iter().find(|m| m.id == "router").unwrap();
    assert_decision(router, 2048, true);
    assert_eq!(router.base_url, definition.base_url);
    assert_decision(
        found.models.iter().find(|m| m.id == "second").unwrap(),
        8192,
        true,
    );
    assert!(!found
        .models
        .iter()
        .any(|m| m.id == "hidden" || m.id == "folder"));
    assert_ne!(
        found
            .models
            .iter()
            .find(|m| m.id == "jev-chat")
            .unwrap()
            .info
            .decision,
        Some(true)
    );
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].url.trim_end_matches('?'),
        "https://local.invalid/prefix/v1/models/status"
    );
    assert_eq!(
        requests[1]
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("authorization"))
            .unwrap()
            .1
            .as_str(),
        "Bearer private-token"
    );
    assert!(!requests[1].headers.contains_key("x-api-key"));
}

#[tokio::test]
async fn mixed_mlx_serve_reads_capabilities_and_authoritative_input_modalities() {
    let definition = mixed_definition("mlx-serve", "https://local.invalid/v1");
    let (found,client)=mixed_fetch(&definition,vec![chat_list(),Reply::json(json!({"data":[
        {"id":"router","capabilities":["decisions"],"max_model_len":32768,"context_length":65536,"input_modalities":["text","image"]},
        {"id":"text-only","capabilities":["decisions"],"context_length":4096,"input_modalities":["text"]},
        {"id":"unknown-input","capabilities":["decisions"]},
        {"id":"jev-chat","capabilities":["chat"]}
    ]}))]).await;
    assert_decision(
        found.models.iter().find(|m| m.id == "router").unwrap(),
        32768,
        true,
    );
    assert_decision(
        found.models.iter().find(|m| m.id == "text-only").unwrap(),
        4096,
        false,
    );
    assert_decision(
        found
            .models
            .iter()
            .find(|m| m.id == "unknown-input")
            .unwrap(),
        0,
        false,
    );
    assert_eq!(
        client.requests.lock().unwrap()[1].url.trim_end_matches('?'),
        "https://local.invalid/v1/models"
    );
}

#[tokio::test]
async fn mixed_ollama_uses_tags_cloud_context_and_model_name_precedence() {
    for (base, cloud_context) in [
        ("https://local.invalid/prefix/v1", 0),
        ("https://ollama.com/v1", 65536),
    ] {
        let definition = mixed_definition("ollama", base);
        let (found,client)=mixed_fetch(&definition,vec![chat_list(),Reply::json(json!({"models":[
            {"model":"router","name":"ignored","capabilities":["decision","vision"],"details":{"context_length":16384}},
            {"name":"fallback-name","capabilities":["decision"]},
            {"model":"remote","remote_host":"https://ollama.com","capabilities":["decision"]},
            {"model":"jev-chat","capabilities":["completion"]}
        ]}))]).await;
        assert_decision(
            found.models.iter().find(|m| m.id == "router").unwrap(),
            16384,
            true,
        );
        assert_decision(
            found
                .models
                .iter()
                .find(|m| m.id == "fallback-name")
                .unwrap(),
            cloud_context,
            false,
        );
        assert_decision(
            found.models.iter().find(|m| m.id == "remote").unwrap(),
            65536,
            false,
        );
        assert_eq!(
            client.requests.lock().unwrap()[1].url.trim_end_matches('?'),
            format!("{}/api/tags", base.trim_end_matches("/v1"))
        );
    }
}

#[tokio::test]
async fn mixed_local_empty_metadata_does_not_guess_jev_as_a_decision_model() {
    for (kind, metadata) in [
        ("omlx", json!({"models":[]})),
        (
            "mlx-serve",
            json!({"data":[],"models":[{"id":"router","capabilities":["decisions"]}]}),
        ),
        ("ollama", json!({"models":[{"name":"router"}]})),
    ] {
        let (found, client) = mixed_fetch(
            &mixed_definition(kind, "https://local.invalid/v1"),
            vec![chat_list(), Reply::json(metadata)],
        )
        .await;
        assert_eq!(client.requests.lock().unwrap().len(), 2, "{kind}");
        assert!(
            found.models.iter().all(|m| m.info.decision != Some(true)),
            "{kind}"
        );
        assert!(found.failed_parts.is_empty());
    }
}

#[tokio::test]
async fn mixed_local_invalid_metadata_marks_only_decision_part_as_failed() {
    for kind in ["omlx", "mlx-serve", "ollama"] {
        let (found, _) = mixed_fetch(
            &mixed_definition(kind, "https://local.invalid/v1"),
            vec![chat_list(), Reply::json(json!({"unexpected":[]}))],
        )
        .await;
        assert_eq!(found.models.len(), 3);
        assert_eq!(found.failed_parts, ["systemone|https://local.invalid/v1"]);
    }
}

#[tokio::test]
async fn mixed_decision_failures_are_bounded_by_the_shared_model_list_deadline() {
    let definition = mixed_definition("omlx", "https://local.invalid/v1");
    let client = MockClient {
        stall_at: Some(2),
        ..client(vec![chat_list()])
    };
    let found = fetch_routes_with_budget(
        &definition,
        &api_credentials(),
        &resolved(),
        &catalog(),
        &client,
        Duration::from_millis(30),
    )
    .await
    .unwrap();
    assert_eq!(found.failed_parts, ["systemone|https://local.invalid/v1"]);
    assert_eq!(found.models.len(), 3);
}

#[tokio::test]
async fn mixed_jev_gateway_classifies_only_complete_jev_path_segments() {
    let definition = mixed_definition("jev", "https://opencode.invalid/zen/v1");
    let ids = [
        "jev",
        "typesafe/jev-fast-v2",
        "JEV-1",
        "lab/jev-2/release",
        "jevons",
        "jevx",
        "router",
        "clef",
    ];
    let (found, client) = mixed_fetch(
        &definition,
        vec![Reply::json(json!({"data":ids.map(|id|json!({"id":id}))}))],
    )
    .await;
    for (i, id) in ids.into_iter().enumerate() {
        let model = found.models.iter().find(|m| m.id == id).unwrap();
        assert_eq!(model.info.decision == Some(true), i < 4, "{id}");
        if i < 4 {
            assert_eq!(model.api, "systemone");
            assert_eq!(model.info.chat, Some(false));
        }
    }
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn mixed_bailian_keeps_named_decision_model_without_making_a_paid_probe() {
    let definition = mixed_definition("bailian", "https://bailian.invalid/compatible-mode/v1");
    let (mut found, client) = mixed_fetch(&definition, vec![chat_list()]).await;
    supplement_models(&definition, Some(&api_credentials()), &mut found.models);
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    let model = found
        .models
        .iter()
        .find(|m| m.id == "decision-model-preview")
        .expect("Bailian decision model");
    assert_decision(model, 0, false);
    assert_eq!(model.name, "Decision model (preview)");
    assert_eq!(model.base_url, definition.base_url);
    let fallback = preset(
        &definition,
        Some(&api_credentials()),
        &ModelsCatalog::default(),
        None,
    )
    .unwrap();
    assert!(fallback["models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["id"] == "decision-model-preview" && m["info"]["chat"] == false));
}

#[test]
fn mixed_decision_request_uses_systemone_protocol_and_keeps_the_original_body() {
    let definition = mixed_definition("omlx", "https://local.invalid/v1");
    let api = crate::transport::request_api(
        &definition,
        &api_credentials(),
        "https://local.invalid/v1/systemone",
    );
    assert_eq!(api, "systemone");
    let original = json!({"model":"router","state":{"text":"hi"},"questions":{"ok":{"type":"noul","instructions":"Is this a greeting?"}}});
    let mut body = Some(serde_json::to_vec(&original).unwrap());
    crate::request_constraints::apply(
        &api,
        &nora_provider_plugin_sdk::RequestConstraints {
            output_token_limit: Some(1),
            reasoning_disabled: true,
            ..Default::default()
        },
        None,
        &mut body,
    )
    .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(body.as_ref().unwrap()).unwrap(),
        original
    );
}

#[test]
fn mixed_decision_packages_declare_discovery_and_account_operation_support() {
    for (package, kind) in [
        ("omlx", "omlx"),
        ("mlx-serve", "mlx-serve"),
        ("ollama", "ollama"),
        ("ollama-cloud", "ollama"),
        ("opencode-zen", "jev"),
        ("bailian-token-plan", "bailian"),
    ] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(package)
            .join("nora-provider.json");
        let manifest: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(manifest["config"]["decisionDiscovery"], kind, "{package}");
        assert_eq!(
            manifest["config"]["operations"],
            json!(["decision"]),
            "{package}"
        );
        assert_eq!(
            manifest["config"]["operationsAuthMethods"]["decision"],
            json!(["api", "apiKey"]),
            "{package}"
        );
    }
}

#[tokio::test]
async fn mixed_local_empty_catalog_is_authoritative_and_removes_old_decision_models() {
    let definition = mixed_definition("omlx", "https://local.invalid/v1");
    let client = client(vec![
        Reply::json(json!({"data":[]})),
        Reply::json(json!({"models":[]})),
    ]);
    let path = std::env::temp_dir().join(format!("nora-mixed-empty-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    let result = discover(&definition, Some(&api_credentials()), &path, &client)
        .await
        .unwrap();
    std::fs::remove_dir_all(path).unwrap();
    assert_eq!(result["models"], json!([]));
    assert_eq!(result["authoritativeEmpty"], true);
    assert_eq!(result["source"], "remote");
    assert_eq!(result["apiEndpoints"]["systemone"], definition.base_url);
}

#[tokio::test]
async fn mixed_local_server_metadata_overrides_cached_decision_labels() {
    let mut definition = mixed_definition("omlx", "https://local.invalid/v1");
    definition.models =
        vec![json!({"id":"router","info":{"decision":true,"chat":false,"toolCalling":false}})];
    let (found, _) = mixed_fetch(
        &definition,
        vec![chat_list(), Reply::json(json!({"models":[]}))],
    )
    .await;
    let model = found
        .models
        .iter()
        .find(|model| model.id == "router")
        .unwrap();
    assert_ne!(model.info.decision, Some(true));
    assert_ne!(model.info.chat, Some(false));
}

#[tokio::test]
async fn mixed_metadata_reads_only_each_servers_declared_fields() {
    for (kind, metadata) in [
        (
            "omlx",
            json!({"models":[{"id":"router","model_type":"decision","capabilities":{"irrelevant":true}}]}),
        ),
        (
            "mlx-serve",
            json!({"data":[{"id":"router","capabilities":["decisions"],"details":"unrelated"}]}),
        ),
        (
            "ollama",
            json!({"models":[{"model":"router","capabilities":["decision"],"model_type":1}]}),
        ),
    ] {
        let (found, _) = mixed_fetch(
            &mixed_definition(kind, "https://local.invalid/v1"),
            vec![chat_list(), Reply::json(metadata)],
        )
        .await;
        assert!(found.failed_parts.is_empty(), "{kind}");
        assert_eq!(
            found
                .models
                .iter()
                .find(|m| m.id == "router")
                .unwrap()
                .info
                .decision,
            Some(true)
        );
    }
}

#[tokio::test]
async fn mixed_model_discovery_cannot_make_a_request_after_its_deadline() {
    let definition = mixed_definition("omlx", "https://local.invalid/v1");
    let client = client(vec![]);
    assert!(fetch_routes_with_budget(
        &definition,
        &api_credentials(),
        &resolved(),
        &catalog(),
        &client,
        Duration::ZERO
    )
    .await
    .is_err());
    assert!(client.requests.lock().unwrap().is_empty());
}
