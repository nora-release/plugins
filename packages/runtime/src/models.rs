use crate::definition::Definition;
use nora_ai::{InputModality, JsonValue, Model, ModelsCatalog};
use serde_json::Value;

#[cfg(test)]
pub fn catalog_models(
    definition: &Definition,
    credentials: Option<&Value>,
) -> Result<Vec<Model>, String> {
    let catalog = ModelsCatalog::bundled().map_err(|_| "The bundled model catalog is invalid")?;
    catalog_models_with_catalog(definition, credentials, &catalog)
}

pub fn catalog_models_with_catalog(
    definition: &Definition,
    credentials: Option<&Value>,
    catalog: &ModelsCatalog,
) -> Result<Vec<Model>, String> {
    let default_catalog = if definition.config.catalog.is_empty() {
        if definition.api == "anthropic-messages" {
            "anthropic"
        } else {
            "openai"
        }
    } else {
        &definition.config.catalog
    };
    let catalog_id = definition.adapter.catalog(credentials, default_catalog);
    let mut models = catalog
        .models_for(catalog_id)
        .into_iter()
        .filter(|model| {
            !model.api.contains("image")
                && !model.api.contains("embedding")
                && !model.api.contains("video")
        })
        .cloned()
        .collect::<Vec<_>>();
    if models.is_empty() {
        models = catalog
            .models_for(&definition.id)
            .into_iter()
            .filter(|model| {
                !model.api.contains("image")
                    && !model.api.contains("embedding")
                    && !model.api.contains("video")
            })
            .cloned()
            .collect();
    }
    for value in definition
        .config
        .model_definitions
        .as_ref()
        .unwrap_or(&definition.models)
    {
        let Some(id) = value
            .as_str()
            .or_else(|| value.get("id").and_then(Value::as_str))
        else {
            continue;
        };
        if models.iter().any(|model| model.id == id) {
            continue;
        }
        let mut model = catalog
            .model(catalog_id, id)
            .cloned()
            .unwrap_or_else(|| Model::new(id, &definition.api, &definition.id));
        if let Some(fields) = value.as_object() {
            let mut normalized = serde_json::to_value(&model).map_err(|error| error.to_string())?;
            normalized
                .as_object_mut()
                .ok_or("Invalid model profile")?
                .extend(fields.clone());
            model = serde_json::from_value(normalized).map_err(|error| error.to_string())?;
        }
        if model.name.is_empty() {
            model.name = id.into();
        }
        models.push(model);
    }
    for model in &mut models {
        model.normalize_info();
    }
    models.sort_by(|left, right| {
        right
            .info
            .release_date
            .cmp(&left.info.release_date)
            .then_with(|| left.id.cmp(&right.id))
    });
    for model in &mut models {
        configure_model(definition, credentials, model);
    }
    models.retain(|model| definition.adapter.retain_model(credentials, model));
    Ok(models)
}

pub fn configure_model(definition: &Definition, credentials: Option<&Value>, model: &mut Model) {
    let endpoint = effective_endpoint(definition, credentials);
    model.provider = definition.id.clone();
    if model.input.is_empty() {
        model.input = vec![InputModality::Text];
    }
    model.base_url = endpoint.clone();
    model.api = definition
        .adapter
        .model_api(credentials, &model.api)
        .unwrap_or_else(|| definition.api.clone());
    if !definition.config.headers.is_empty() {
        model
            .headers
            .get_or_insert_with(Default::default)
            .extend(definition.config.headers.clone());
    }
    model
        .query_params
        .extend(definition.config.query_params.clone());
    model.provider_options.extend(
        definition
            .config
            .provider_options
            .iter()
            .map(|(key, value)| (key.clone(), JsonValue::from(value.clone()))),
    );
    if definition.config.endpoint_kind.as_deref() == Some("region") {
        model
            .provider_options
            .insert("region".into(), JsonValue::string(&definition.base_url));
    }
    model.normalize_info();
}

pub fn effective_endpoint(definition: &Definition, credentials: Option<&Value>) -> String {
    if let Some(endpoint) = definition.adapter.endpoint(credentials) {
        return endpoint;
    }
    if definition.config.endpoint_kind.as_deref() == Some("region") {
        return format!(
            "https://bedrock-runtime.{}.amazonaws.com",
            definition.base_url
        );
    }
    definition.base_url.clone()
}

#[cfg(test)]
pub(crate) use crate::xai_provider::grok_model_selection;

#[cfg(test)]
mod tests {
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
    fn every_package_has_models_and_an_independent_manifest() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let mut count = 0;
        for entry in std::fs::read_dir(root).unwrap() {
            let manifest = entry.unwrap().path().join("nora-provider.json");
            if !manifest.exists() {
                continue;
            }
            let definition = Definition::parse(
                serde_json::from_slice(&std::fs::read(manifest).unwrap()).unwrap(),
            )
            .unwrap();
            assert!(
                !catalog_models(&definition, None).unwrap().is_empty(),
                "{}",
                definition.id
            );
            count += 1;
        }
        assert_eq!(count, 26);
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
        assert!(!models.is_empty());
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
        assert!(api
            .iter()
            .all(|model| !model.id.starts_with("grok-build")
                && !model.id.starts_with("grok-composer")));
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
}
