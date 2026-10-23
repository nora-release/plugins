use super::*;
use serde_json::json;
fn definition(id: &str) -> Definition {
    let mut definition = Definition::parse(
        serde_json::from_slice(
            &std::fs::read(format!(
                "{}/{id}/nora-provider.json",
                env!("CARGO_MANIFEST_DIR").trim_end_matches("/runtime")
            ))
            .unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    definition.adapter = crate::test_adapter(id);
    definition
}
#[test]
fn native_packages_keep_their_own_schema_and_generic_packages_have_models_or_discovery() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let mut ids = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let directory = entry.unwrap().path();
        let manifest = directory.join("nora-provider.json");
        if !manifest.exists() {
            continue;
        }
        let value: Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        let id = crate::definition::validate_package(&value).unwrap();
        assert!(
            ids.insert(id.to_owned()),
            "Duplicate provider package identity: {id}"
        );
        if directory.join("Cargo.toml").is_file() {
            continue;
        }
        let definition = Definition::parse(value).unwrap();
        let bundled = catalog_models(&definition, None).unwrap();
        let remote = definition
            .config
            .model_discovery
            .as_ref()
            .is_some_and(|discovery| {
                !discovery.no_list
                    && !discovery.bases.is_empty()
                    && discovery.bases.iter().all(|base| {
                        url::Url::parse(&base.base_url).is_ok_and(|url| {
                            matches!(url.scheme(), "https" | "http") && url.host_str().is_some()
                        })
                    })
            });
        assert!(
            !bundled.is_empty() || remote,
            "{} has neither models nor a secure discovery route",
            definition.id
        );
    }
    assert!(ids.len() >= 38);
    for id in ["openai", "anthropic", "google-gemini", "amazon-bedrock"] {
        assert!(ids.contains(id));
    }
}
#[test]
fn account_types_choose_the_subscription_protocol_and_endpoint() {
    let openai = definition("openai");
    let models = catalog_models(&openai, Some(&json!({"type":"oauth"}))).unwrap();
    assert!(models
        .iter()
        .all(|model| model.api == "chatgpt-codex" && model.base_url == "https://chatgpt.com"));
    assert!(catalog_models(&openai, Some(&json!({"type":"apiKey"})))
        .unwrap()
        .iter()
        .all(|model| model.api == "openai-responses"));
}

#[test]
fn prepared_api_models_do_not_change_the_oauth_model_catalog() {
    let mut openai = definition("openai");
    let expected = catalog_models(&openai, Some(&json!({"type":"oauth"}))).unwrap();
    openai.models = catalog_models(&openai, None)
        .unwrap()
        .into_iter()
        .map(|model| serde_json::to_value(model).unwrap())
        .collect();
    openai.config.model_definitions = Some(Vec::new());
    let actual = catalog_models(&openai, Some(&json!({"type":"oauth"}))).unwrap();
    assert_eq!(
        actual.iter().map(|model| &model.id).collect::<Vec<_>>(),
        expected.iter().map(|model| &model.id).collect::<Vec<_>>()
    );
}

#[test]
fn custom_packages_without_a_catalog_use_their_own_protocol_and_identity() {
    let custom=Definition::parse(json!({"id":"personal","name":"Personal","api":"anthropic-messages","baseURL":"https://personal.invalid","models":[],"config":{"keyProvider":"personal","nativeAPI":"anthropic-messages"}})).unwrap();
    let models = catalog_models(&custom, None).unwrap();
    assert!(
        models.is_empty(),
        "An unknown provider must not inherit another provider catalog"
    );
    assert!(models.iter().all(|model| model.provider == "personal"
        && model.api == "anthropic-messages"
        && model.base_url == "https://personal.invalid"));
}

#[test]
fn changing_a_bedrock_region_uses_the_current_provider_configuration() {
    let mut bedrock = definition("amazon-bedrock");
    bedrock.base_url = "eu-west-1".into();
    let credentials = json!({"type":"apiKey","extras":{"baseUrl":"us-east-1"}});
    let endpoint = "https://bedrock-runtime.eu-west-1.amazonaws.com";
    assert_eq!(effective_endpoint(&bedrock, Some(&credentials)), endpoint);
    assert!(catalog_models(&bedrock, Some(&credentials))
        .unwrap()
        .iter()
        .all(|model| model.base_url == endpoint
            && model
                .provider_options
                .get("region")
                .and_then(|value| value.as_str())
                == Some("eu-west-1")));
}

#[test]
fn azure_models_supply_the_api_version_from_the_provider_package() {
    let azure = definition("azure-openai-responses");
    assert!(catalog_models(&azure, None)
        .unwrap()
        .iter()
        .all(|model| model
            .provider_options
            .get("apiVersion")
            .and_then(|value| value.as_str())
            == Some("2024-10-21")));
}

#[test]
fn manually_configured_routes_use_the_connected_account_protocol_and_endpoint() {
    let openai = definition("openai");
    for (kind, api, endpoint) in [
        ("oauth", "chatgpt-codex", "https://chatgpt.com"),
        ("apiKey", "openai-responses", "https://api.openai.com"),
    ] {
        let mut model = Model::new("future-chat-model", &openai.api, &openai.id);
        configure_model(&openai, Some(&json!({"type":kind})), &mut model);
        assert_eq!(model.api, api);
        assert_eq!(model.base_url, endpoint);
        assert_eq!(model.input, vec![InputModality::Text]);
    }
}

#[test]
fn manually_configured_kimi_routes_keep_the_provider_identity_headers() {
    let kimi = definition("kimi-coding");
    let mut model = Model::new("future-kimi-model", &kimi.api, &kimi.id);
    configure_model(&kimi, Some(&json!({"type":"apiKey"})), &mut model);
    assert_eq!(model.api, "anthropic-messages");
    assert_eq!(model.base_url, "https://api.kimi.com/coding");
    assert_eq!(model.headers.as_ref().unwrap()["User-Agent"], "KimiCLI/1.5");
}

#[test]
fn manually_added_api_models_cannot_enable_grok_subscription_models() {
    let mut xai = definition("xai");
    xai.config.model_definitions = Some(vec![
        json!("grok-build-custom"),
        json!("grok-composer-custom"),
        json!("grok-future-api"),
    ]);
    let api = catalog_models(&xai, Some(&json!({"type":"apiKey"}))).unwrap();
    assert!(api.iter().any(|model| model.id == "grok-future-api"));
    assert!(
        api.iter()
            .all(|model| !model.id.starts_with("grok-build")
                && !model.id.starts_with("grok-composer"))
    );
    let oauth = catalog_models(&xai, Some(&json!({"type":"oauth"}))).unwrap();
    assert!(oauth.iter().any(|model| model.id == "grok-build-custom"
        && model.base_url == nora_provider_auth::XAI_OAUTH_PROXY_BASE_URL));
}

#[test]
fn selected_ids_do_not_become_models_reported_by_the_provider() {
    let parsed = Definition::parse(json!({
        "id":"personal","name":"Personal","api":"openai-responses",
        "baseURL":"https://personal.invalid",
        "config":{"keyProvider":"personal","modelIDs":["future-chat-model"]}
    }))
    .unwrap();
    assert!(!catalog_models(&parsed, Some(&json!({"type":"apiKey"})))
        .unwrap()
        .iter()
        .any(|model| model.id == "future-chat-model"));
}

fn catalog_models(
    definition: &Definition,
    credentials: Option<&Value>,
) -> Result<Vec<Model>, String> {
    let catalog = ModelsCatalog::bundled().map_err(|_| "The bundled model catalog is invalid")?;
    catalog_models_with_catalog(definition, credentials, &catalog)
}

#[test]
fn media_models_keep_operation_capabilities_out_of_chat_options() {
    for (id, entry, image, video, embedding, chat) in [
        (
            "gpt-image-2",
            serde_json::json!({}),
            true,
            false,
            false,
            false,
        ),
        (
            "grok-imagine-video",
            serde_json::json!({}),
            false,
            true,
            false,
            false,
        ),
        (
            "text-embedding-3-small",
            serde_json::json!({}),
            false,
            false,
            true,
            false,
        ),
        (
            "gemini-image",
            serde_json::json!({"supportedGenerationMethods":["generateContent"],"modalities":{"output":["text","image"]}}),
            true,
            false,
            false,
            true,
        ),
    ] {
        let mut model = Model::new(id, "openai-completions", "custom");
        assert!(apply_media_metadata(&entry, &mut model));
        assert_eq!(model.info.image_output.unwrap_or(false), image);
        assert_eq!(model.info.video_output.unwrap_or(false), video);
        assert_eq!(model.info.embedding.unwrap_or(false), embedding);
        assert_eq!(model.info.chat, Some(chat));
        assert!(model.provider_options.is_empty());
    }
}

#[test]
fn subscription_media_presets_are_explicit_and_remote_models_win() {
    let definition = definition("openai");
    let credentials = serde_json::json!({"type":"oauth"});
    let mut models = vec![Model::new("live-model", "chatgpt-codex", "openai")];
    crate::discovery::supplement_models(&definition, Some(&credentials), &mut models);
    let image = models.iter().find(|m| m.id == "gpt-image-2").unwrap();
    assert_eq!(image.info.catalog_source.as_deref(), Some("preset"));
    assert_eq!(image.info.chat, Some(false));
    assert_eq!(image.info.image_output, Some(true));
    let original = models.clone();
    crate::discovery::supplement_models(&definition, Some(&credentials), &mut models);
    assert_eq!(models, original);
}

#[test]
fn supplier_capabilities_take_precedence_over_name_heuristics() {
    let mut embedding = Model::new("semantic-vector-v2", "google-generative-ai", "google");
    embedding.info.embedding = Some(true);
    embedding.info.chat = Some(false);
    assert!(apply_media_metadata(&serde_json::json!({}), &mut embedding));
    assert_eq!(embedding.info.embedding, Some(true));
    let mut chat = Model::new("gpt-image-explanation", "openai-completions", "custom");
    chat.info.image_output = Some(false);
    chat.info.chat = Some(true);
    assert!(!apply_media_metadata(&serde_json::json!({}), &mut chat));
    assert_eq!(chat.info.image_output, Some(false));
    assert_eq!(chat.info.chat, Some(true));
}
