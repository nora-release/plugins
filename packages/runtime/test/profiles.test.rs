use super::*;
use crate::{BundledProvider, GenericProvider};
use nora_ai::{InputModality, ModelsCatalog};
use nora_provider_plugin_sdk::{InitializeParams, ProviderPlugin};
use serde_json::json;

fn definition() -> Definition {
    Definition::parse(json!({"id":"profiles","name":"Profiles","api":"openai-completions","baseURL":"https://plan.invalid/v1","authMethods":[{"id":"apiKey","type":"api","label":"Plan"},{"id":"profile-api","type":"api","label":"API"}],"config":{"keyProvider":"profiles","profiles":[{"id":"plan","name":"Token Plan","catalog":"plan-prices,api-prices","noList":true,"models":[{"id":"claude-current"}],"bases":[{"api":"openai-completions","baseURL":"https://plan.invalid/v1"},{"api":"anthropic-messages","baseURL":"https://plan.invalid/anthropic"}],"modelRoutes":[{"pattern":"*claude*","api":"anthropic-messages"}]},{"id":"api","name":"Pay as you go","catalog":"api-prices","bases":[{"api":"openai-completions","baseURL":"https://api.invalid/v1"}]}]}})).unwrap()
}
fn manifest() -> Value {
    json!({"id":"profiles","name":"Profiles","api":"openai-completions","baseURL":"https://plan.invalid/v1","authMethods":[{"id":"apiKey","type":"api","label":"Plan"},{"id":"profile-api","type":"api","label":"API"}],"config":{"keyProvider":"profiles","profiles":[{"id":"plan","name":"Token Plan","models":[{"id":"claude-current"}],"noList":true,"bases":[{"api":"openai-completions","baseURL":"https://plan.invalid/v1"}]},{"id":"api","name":"API","bases":[{"api":"openai-completions","baseURL":"https://api.invalid/v1"}]}]}})
}

#[test]
fn profile_selection_changes_only_its_account_and_removed_profiles_never_fall_back_to_billing() {
    let original = definition();
    let plan = configured(original.clone(), Some(&json!({"profile":"plan"}))).unwrap();
    let api = configured(original.clone(), Some(&json!({"extras":{"profile":"api"}}))).unwrap();
    assert_eq!(plan.base_url, "https://plan.invalid/v1");
    assert_eq!(api.base_url, "https://api.invalid/v1");
    assert!(plan.config.model_discovery.unwrap().no_list);
    assert!(!api.config.model_discovery.unwrap().no_list);
    assert_eq!(original.base_url, "https://plan.invalid/v1");
    assert!(configured(original, Some(&json!({"profile":"removed"}))).is_err());
}

#[test]
fn plan_models_keep_native_protocols_and_do_not_include_unrelated_catalog_models() {
    let plan = configured(definition(), Some(&json!({"profile":"plan"}))).unwrap();
    let mut catalog = ModelsCatalog::default();
    let mut native = Model::new("claude-current", "openai-completions", "plan-prices");
    native.input = vec![InputModality::Text, InputModality::Image];
    catalog.insert(native);
    catalog.insert(Model::new(
        "unrelated-paid-model",
        "openai-completions",
        "api-prices",
    ));
    let models =
        crate::models::catalog_models_with_catalog(&plan, Some(&json!({"type":"api"})), &catalog)
            .unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].api, "anthropic-messages");
    assert_eq!(models[0].base_url, "https://plan.invalid/anthropic");
    assert_eq!(
        models[0].input,
        vec![InputModality::Text, InputModality::Image]
    );
}

#[test]
fn endpoint_selection_requires_explicit_account_input() {
    let mut definition = definition();
    definition.config.profiles[1].custom_endpoint = true;
    definition.config.profiles[1].endpoint_required = true;
    let mut credentials = OAuthCredentials::api_key(nora_ai::OAuthProviderId::ApiKey, "key");
    assert!(assign(&definition, "profile-api", &json!({}), &mut credentials).is_err());
    assign(
        &definition,
        "profile-api",
        &json!({"endpoint":"http://127.0.0.1:1234/v1"}),
        &mut credentials,
    )
    .unwrap();
    let saved = serde_json::to_value(&credentials).unwrap();
    let configured = configured(definition.clone(), Some(&saved)).unwrap();
    assert_eq!(configured.base_url, "http://127.0.0.1:1234/v1");
    assert!(super::configured(definition, Some(&json!({"profile":"api","access":"key"}))).is_err());
}

fn local_definition(id: &str) -> Definition {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(id)
        .join("nora-provider.json");
    Definition::parse(serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()).unwrap()
}

#[test]
fn local_service_addresses_preserve_every_api_path() {
    for id in ["ollama", "omlx", "mlx-serve", "lmstudio"] {
        let original = local_definition(id);
        for (input, origin) in [
            ("http://127.0.0.1:9000/v1", "http://127.0.0.1:9000"),
            ("localhost:11435", "http://localhost:11435"),
            ("192.168.1.5:11434", "http://192.168.1.5:11434"),
            ("myhost", "http://myhost"),
            (" http://box:9000/api/?x=1#y ", "http://box:9000"),
            ("https://local.example.com", "https://local.example.com"),
            ("[::1]:11434", "http://[::1]:11434"),
            ("HTTP://Box:1", "http://box:1"),
        ] {
            let mut credentials = OAuthCredentials::api_key(nora_ai::OAuthProviderId::ApiKey, "");
            assign(
                &original,
                "apiKey",
                &json!({"endpoint":input}),
                &mut credentials,
            )
            .unwrap_or_else(|error| panic!("{id} {input}: {error}"));
            let saved = serde_json::to_value(&credentials).unwrap();
            let moved = configured(original.clone(), Some(&saved))
                .unwrap_or_else(|error| panic!("{id} {input}: {error}"));
            let bases = moved.config.model_discovery.unwrap().bases;
            assert_eq!(bases.len(), original.config.profiles[0].bases.len());
            for (base, prior) in bases.iter().zip(&original.config.profiles[0].bases) {
                let path = url::Url::parse(&prior.base_url).unwrap().path().to_owned();
                assert_eq!(base.api, prior.api);
                assert_eq!(
                    base.base_url,
                    format!("{origin}{path}").trim_end_matches('/'),
                    "{id} {input}"
                );
            }
            assert_eq!(moved.base_url, bases[0].base_url);
            // Values already saved by older releases must use the same interpretation.
            let legacy = configured(original.clone(), Some(&json!({"endpoint":input}))).unwrap();
            assert_eq!(legacy.base_url, moved.base_url);
        }
    }
}

#[test]
fn local_service_addresses_discard_private_userinfo_and_pasted_query_before_saving() {
    let original = local_definition("omlx");
    let mut credentials = OAuthCredentials::api_key(nora_ai::OAuthProviderId::ApiKey, "");
    assign(
        &original,
        "apiKey",
        &json!({"endpoint":"https://private:secret@box:8443/api?token=private#fragment"}),
        &mut credentials,
    )
    .unwrap();
    let saved = serde_json::to_value(&credentials).unwrap();
    assert_eq!(saved["extras"]["endpoint"], "https://box:8443");
    let moved = configured(original, Some(&saved)).unwrap();
    for base in moved.config.model_discovery.unwrap().bases {
        assert!(base.base_url.starts_with("https://box:8443"));
        assert!(!base.base_url.contains("private"));
        assert!(!base.base_url.contains("secret"));
    }
}

#[test]
fn local_service_addresses_reject_invalid_authorities_and_keep_optional_defaults() {
    let original = local_definition("ollama");
    for input in [
        "ftp://box:9000",
        "http://",
        ":11434",
        "my ollama box",
        "http://my host:11434",
        "localhost:abc",
        "localhost:70000",
        "http://localhost:0",
        "http:///missing-host",
        "box:9000/path\npart",
    ] {
        let mut credentials = OAuthCredentials::api_key(nora_ai::OAuthProviderId::ApiKey, "");
        assert!(
            assign(
                &original,
                "apiKey",
                &json!({"endpoint":input}),
                &mut credentials
            )
            .is_err(),
            "{input}"
        );
        assert!(
            configured(original.clone(), Some(&json!({"endpoint":input}))).is_err(),
            "{input}"
        );
    }
    for input in ["", "  ", "\t"] {
        let moved = configured(original.clone(), Some(&json!({"endpoint":input}))).unwrap();
        assert_eq!(moved.base_url, original.base_url);
    }
}

#[tokio::test]
async fn explicit_api_profile_authorization_persists_the_choice_and_optional_keys_reject_bad_values(
) {
    let plugin = BundledProvider::new(GenericProvider);
    plugin
        .initialize(InitializeParams {
            subscription_accounts_enabled: true,
            installed_manifest: None,
            runtime_config: serde_json::Value::Null,
            api_version: 1,
            plugin_id: "profiles".into(),
            plugin_directory: "/tmp/profiles".into(),
            data_directory: "/tmp/profiles".into(),
            manifest: manifest(),
        })
        .await
        .unwrap();
    let connected = plugin
        .authorize(json!({"authMethodID":"profile-api","inputs":{"apiKey":"own-key"}}))
        .await
        .unwrap();
    assert_eq!(connected["credentials"]["authMethod"], "profile-api");
    let saved = &connected["credentials"];
    let profile = configured(definition(), Some(saved)).unwrap();
    assert_eq!(profile.base_url, "https://api.invalid/v1");
    let mut optional = manifest();
    optional["config"]["profiles"][0]["keyOptional"] = json!(true);
    let plugin = BundledProvider::new(GenericProvider);
    plugin
        .initialize(InitializeParams {
            subscription_accounts_enabled: true,
            installed_manifest: None,
            runtime_config: serde_json::Value::Null,
            api_version: 1,
            plugin_id: "profiles".into(),
            plugin_directory: "/tmp/profiles".into(),
            data_directory: "/tmp/profiles".into(),
            manifest: optional,
        })
        .await
        .unwrap();
    assert!(plugin.authorize(json!({"inputs":{}})).await.is_ok());
    assert!(plugin
        .authorize(json!({"inputs":{"apiKey":"key\r\nInjected: bad"}}))
        .await
        .is_err());
    assert!(plugin
        .authorize(json!({"inputs":{"apiKey":123}}))
        .await
        .is_err());
}

#[test]
fn profile_schema_rejects_duplicate_or_unsafe_routes_before_any_account_is_used() {
    let mut definition = definition();
    let duplicate = definition.config.profiles[0].clone();
    definition.config.profiles.push(duplicate);
    assert!(validate(&definition).is_err());
    definition = super::tests::definition();
    definition.config.profiles[0].bases[0].base_url = "https://key@example.invalid/v1".into();
    assert!(validate(&definition).is_err());
}

#[test]
fn migrated_accounts_keep_their_previous_plan_until_reconnected_explicitly() {
    let mut original = definition();
    original.config.legacy_profile = Some("api".into());
    assert_eq!(
        configured(
            original.clone(),
            Some(&json!({"type":"apiKey","access":"legacy"}))
        )
        .unwrap()
        .base_url,
        "https://api.invalid/v1"
    );
    assert_eq!(
        configured(original.clone(), None).unwrap().base_url,
        "https://plan.invalid/v1"
    );
    assert_eq!(
        configured(original, Some(&json!({"profile":"plan"})))
            .unwrap()
            .base_url,
        "https://plan.invalid/v1"
    );
}

#[test]
fn api_profiles_match_the_upstream_provider_regions_and_plans() {
    let upstream: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/magpie-provider-presets.json")).unwrap();
    for expected in upstream {
        let id = expected["ID"].as_str().unwrap();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../")
            .join(id)
            .join("nora-provider.json");
        if !path.exists() {
            continue;
        }
        let raw: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        if raw.pointer("/config/profiles").is_none() {
            continue;
        }
        let definition = Definition::parse(raw).unwrap();
        let regions = expected["Regions"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| vec![json!({"ID":"default"})]);
        assert_eq!(definition.config.profiles.len(), regions.len(), "{id}");
        for region in regions {
            let profile = definition
                .config
                .profiles
                .iter()
                .find(|p| Some(p.id.as_str()) == region["ID"].as_str())
                .unwrap();
            for (field, api) in [
                ("Chat", "openai-completions"),
                ("Responses", "openai-responses"),
                ("Anthropic", "anthropic-messages"),
            ] {
                let url = region
                    .get(field)
                    .or_else(|| expected.get(field))
                    .and_then(Value::as_str);
                assert_eq!(
                    profile
                        .bases
                        .iter()
                        .find(|base| base.api == api)
                        .map(|b| b.base_url.as_str()),
                    url,
                    "{id}/{} {api}",
                    profile.id
                );
            }
            assert_eq!(
                profile.no_list,
                expected["NoList"].as_bool().unwrap_or(false)
                    && !region["Lists"].as_bool().unwrap_or(false),
                "{id}/{}",
                profile.id
            );
        }
    }
}
