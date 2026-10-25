use super::*;
use nora_ai::ModelCost;
use serde_json::json;

fn model(provider: &str, id: &str, input: f64) -> Model {
    let mut model = Model::new(id, "openai-completions", provider);
    model.cost = ModelCost {
        input,
        output: input * 4.0,
        cache_read: input / 10.0,
        cache_write: input * 1.25,
    };
    model.info.pricing_known = Some(true);
    model
}

fn query(id: &str, same: Option<&str>) -> Query {
    Query {
        id: id.into(),
        same_as: same.map(str::to_owned),
    }
}

fn catalog(models: Vec<Model>) -> ModelsCatalog {
    let mut catalog = ModelsCatalog::default();
    for model in models {
        catalog.insert(model);
    }
    catalog
}

#[test]
fn maker_exact_price_wins_over_resellers_and_equivalent_aliases() {
    let catalog = catalog(vec![
        model("openrouter", "gpt-6-sol", 20.0),
        model("openai", "gpt-6-sol", 2.0),
        model("anthropic", "claude-opus-5", 5.0),
    ]);
    let response = resolve(
        &catalog,
        &[
            query("gpt-6-sol", Some("claude-opus-5")),
            query("qfmodel", Some("openai/GPT-6-Sol")),
        ],
    );
    assert_eq!(response.models["gpt-6-sol"].price.input, 2.0);
    assert_eq!(response.defaults["gpt-6-sol"].price.input, 2.0);
    assert_eq!(response.models["qfmodel"].price.input, 2.0);
    assert_eq!(response.models["qfmodel"].canonical, "openai/gpt-6-sol");
    assert!(!response.defaults.contains_key("qfmodel"));
}

#[test]
fn unknown_aliases_and_membership_or_reseller_tariffs_stay_unpriced() {
    let mut reseller = model("openrouter", "arbitrary", 30.0);
    reseller.info.canonical_model_id = Some("openai/gpt-missing".into());
    let catalog = catalog(vec![
        reseller,
        model("kimi-code-plan-cn", "kimi-member", 0.0),
        model("xiaomi-token-plan-cn", "mimo-member", 0.0),
        model("groq", "hosted-model", 1.0),
        model("openai", "unrelated", 3.0),
    ]);
    for alias in [
        "arbitrary",
        "gpt-missing",
        "kimi-member",
        "mimo-member",
        "hosted-model",
        "unlisted",
    ] {
        assert!(
            resolve(&catalog, &[query("qfmodel", Some(alias))])
                .models
                .is_empty(),
            "{alias}"
        );
    }
}

#[test]
fn exact_known_free_is_a_price_but_missing_zero_is_not() {
    let mut unknown = model("openai", "unknown", 0.0);
    unknown.info.pricing_known = None;
    let catalog = catalog(vec![
        model("openai", "free", 0.0),
        unknown,
        model("anthropic", "paid", 5.0),
    ]);
    let result = resolve(
        &catalog,
        &[query("free", Some("paid")), query("unknown", None)],
    );
    assert_eq!(result.models["free"].price.input, 0.0);
    assert!(!result.models.contains_key("unknown"));
}

#[test]
fn canonical_prices_require_the_same_maker_and_every_variant_must_agree() {
    let mut first = model("deepseek", "deepseek-flash", 0.2);
    first.info.canonical_model_id = Some("deepseek/deepseek-v4.1-flash".into());
    let mut second = first.clone();
    second.id = "deepseek-flash-fast".into();
    let mut reseller = model("xiaomi", "mimo-fast", 20.0);
    reseller.info.canonical_model_id = Some("deepseek/deepseek-v4.1-flash".into());
    let consistent = catalog(vec![first.clone(), second.clone(), reseller.clone()]);
    let metadata = &resolve(
        &consistent,
        &[query("qfmodel", Some("deepseek-v4.1-flash"))],
    )
    .models["qfmodel"];
    assert_eq!(metadata.price.input, 0.2);
    assert_eq!(metadata.canonical, "deepseek/deepseek-v4.1-flash");
    second.cost.input = 0.4;
    let conflicting = catalog(vec![first, second, reseller]);
    assert!(resolve(
        &conflicting,
        &[query("qfmodel", Some("deepseek-v4.1-flash"))]
    )
    .models
    .is_empty());
}

#[test]
fn canonical_free_variants_cannot_supply_an_unknown_models_paid_price() {
    let mut free = model("xiaomi", "mimo-v2.6-flash-free", 0.0);
    free.info.canonical_model_id = Some("xiaomi/mimo-v2.6-flash".into());
    assert!(resolve(
        &catalog(vec![free]),
        &[query("qfmodel", Some("mimo-v2.6-flash"))]
    )
    .models
    .is_empty());
}

#[test]
fn missing_and_explicit_zero_hour_prices_are_the_same_canonical_quote() {
    let mut first = model("deepseek", "first", 1.0);
    first.info.canonical_model_id = Some("deepseek/real-model".into());
    first.info.pricing_tiers = vec![serde_json::from_value(
        json!({"above":1000,"input":2,"output":8,"cacheRead":0.2,"cacheWrite":2.5}),
    )
    .unwrap()];
    let mut second = first.clone();
    second.id = "second".into();
    second.info.pricing_tiers[0].cache_write_1h = Some(serde_json::Number::from(0));
    let result = resolve(
        &catalog(vec![first, second]),
        &[query("alias", Some("deepseek/real-model"))],
    );
    assert_eq!(result.models["alias"].price.input, 1.0);
}

#[test]
fn vendor_spellings_variants_profiles_and_proven_relay_renames_follow_magpie() {
    let catalog = catalog(vec![
        model("anthropic", "claude-opus-4-6", 5.0),
        model("anthropic", "anthropic.claude-opus-5-5", 6.0),
        model("deepseek", "deepseek-v4.1-flash", 0.2),
        model("openai", "gpt-6-sol", 2.0),
    ]);
    for (alias, expected) in [
        ("Anthropic/Claude-Opus-4.6", 5.0),
        ("gpt-6-sol(high)", 2.0),
        ("gpt-6-sol:batch", 2.0),
        ("apac.anthropic.claude-opus-5-5", 6.0),
        ("nyro-gpt-6-sol", 2.0),
        ("claude-deepseek-v4.1-flash[1.5m]", 0.2),
    ] {
        assert_eq!(
            resolve(&catalog, &[query("qfmodel", Some(alias))]).models["qfmodel"]
                .price
                .input,
            expected,
            "{alias}"
        );
    }
    for alias in [
        "claude-auto",
        "gpt-6-sol-20261025",
        "claude-deepseek-unlisted[1m]",
    ] {
        assert!(
            resolve(&catalog, &[query("qfmodel", Some(alias))])
                .models
                .is_empty(),
            "{alias}"
        );
    }
}

#[test]
fn listed_relay_rename_does_not_inherit_an_unrelated_makers_price() {
    let catalog = catalog(vec![
        model("openrouter", "claude-deepseek-v4.1-flash", 12.0),
        model("deepseek", "deepseek-v4.1-flash", 0.2),
    ]);
    assert!(resolve(
        &catalog,
        &[query("qfmodel", Some("claude-deepseek-v4.1-flash"))]
    )
    .models
    .is_empty());
}

#[test]
fn grok_documented_window_and_effort_names_keep_fast_variants_distinct() {
    let catalog = catalog(vec![
        model("xai", "grok-4.7", 3.0),
        model("openai", "gpt-5.6-luna", 0.5),
    ]);
    for alias in ["grok-4.7@500k", "grok-4.7-low", "grok-4.7-extra-high"] {
        assert_eq!(
            resolve(&catalog, &[query("qfmodel", Some(alias))]).models["qfmodel"]
                .price
                .input,
            3.0
        );
    }
    for alias in ["grok-4.7-low-fast", "grok-4.7-fast", "grok-4.7-build"] {
        assert!(resolve(&catalog, &[query("qfmodel", Some(alias))])
            .models
            .is_empty());
    }
    assert_eq!(
        resolve(&catalog, &[query("codex-auto-review", None)]).models["codex-auto-review"]
            .price
            .input,
        0.5
    );
}

#[test]
fn canonical_conflicts_include_one_hour_cache_and_tier_differences() {
    let mut first = model("anthropic", "version-a", 3.0);
    first.info.canonical_model_id = Some("anthropic/claude-latest".into());
    first.info.cache_write_1h = Some(serde_json::Number::from(6));
    let mut second = first.clone();
    second.id = "version-b".into();
    second.info.cache_write_1h = Some(serde_json::Number::from(7));
    assert!(resolve(
        &catalog(vec![first.clone(), second.clone()]),
        &[query("qfmodel", Some("claude-latest"))]
    )
    .models
    .is_empty());
    second.info.cache_write_1h = first.info.cache_write_1h.clone();
    second.info.pricing_tiers = vec![serde_json::from_value(
        json!({"above":1000,"input":6,"output":24,"cacheRead":0.6,"cacheWrite":7.5}),
    )
    .unwrap()];
    assert!(resolve(
        &catalog(vec![first, second]),
        &[query("qfmodel", Some("claude-latest"))]
    )
    .models
    .is_empty());
}

struct Directory(std::path::PathBuf);
impl Directory {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("nora-model-metadata-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        Self(directory)
    }
    fn save(&self, input: f64) {
        std::fs::write(self.0.join("models.json"),serde_json::to_vec(&json!({"anthropic":{"models":{"claude-opus-5":{"id":"claude-opus-5","canonical_model_id":"anthropic/claude-opus-5","cost":{"input":input,"output":25,"cache_read":0.5,"cache_write":6.25,"cache_write_1h":11,"tiers":[{"above":272000,"input":10,"output":37.5,"cache_write_1h":22}]}}}}})).unwrap()).unwrap();
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn cached_metadata_reads_latest_catalog_and_preserves_one_hour_and_tier_prices() {
    let directory = Directory::new();
    let params = json!({"models":[{"id":"qfmodel","sameAs":"claude-opus-5"}],"subscriptionAccountsEnabled":false});
    assert_eq!(
        cached(&directory.0, &params).unwrap(),
        json!({"models":{},"defaults":{}})
    );
    directory.save(5.0);
    let first = cached(&directory.0, &params).unwrap();
    assert_eq!(
        first["models"]["qfmodel"]["canonical"],
        "anthropic/claude-opus-5"
    );
    assert_eq!(first["models"]["qfmodel"]["price"]["input"], 5.0);
    assert_eq!(first["models"]["qfmodel"]["price"]["cacheWrite1h"], 11.0);
    assert_eq!(
        first["models"]["qfmodel"]["price"]["tiers"][0]["above"],
        272000
    );
    assert_eq!(
        first["models"]["qfmodel"]["price"]["tiers"][0]["cacheWrite1h"],
        22.0
    );
    assert!(first["defaults"].as_object().unwrap().is_empty());
    directory.save(7.0);
    assert_eq!(
        cached(&directory.0, &params).unwrap()["models"]["qfmodel"]["price"]["input"],
        7.0
    );
}

#[test]
fn metadata_accepts_only_ids_and_never_echoes_invalid_credentials_or_rows() {
    let directory = Directory::new();
    for params in [
        json!({"models":[{"id":"qfmodel","credentials":"private-value"}]}),
        json!({"models":[{"id":"qfmodel"}],"credentials":{"access":"private-value"}}),
        json!({"models":[{"id":"qfmodel"},{"id":"qfmodel"}]}),
        json!({"models":[{"id":"qfmodel","sameAs":"vendor/"}]}),
    ] {
        let error = cached(&directory.0, &params).unwrap_err().to_string();
        assert!(!error.contains("private-value"));
    }
    directory.save(5.0);
    let result = cached(
        &directory.0,
        &json!({"models":[{"id":"qfmodel","sameAs":"claude-opus-5"}]}),
    )
    .unwrap();
    assert_eq!(
        result["models"]["qfmodel"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["canonical", "price"]
    );
    assert!(!result.to_string().contains("canonical_model_id"));
}

#[tokio::test]
async fn bundled_plugin_declares_and_serves_private_cached_metadata_without_accounts_or_network() {
    use nora_provider_plugin_sdk::{InitializeParams, ProviderPlugin, API_VERSION};
    let directory = Directory::new();
    directory.save(5.0);
    let provider = crate::BundledProvider::new(crate::openai_provider::OpenAI);
    let initialized = provider
        .initialize(InitializeParams {
            api_version: API_VERSION,
            plugin_id: "openai".into(),
            plugin_directory: Default::default(),
            data_directory: directory.0.clone(),
            manifest: serde_json::from_str(include_str!("../../openai/nora-provider.json"))
                .unwrap(),
            installed_manifest: None,
            runtime_config: Value::Null,
            subscription_accounts_enabled: false,
        })
        .await
        .unwrap();
    assert_eq!(initialized["capabilities"]["modelMetadata"], true);
    let result=provider.call("modelMetadata",json!({"models":[{"id":"qfmodel","sameAs":"claude-opus-5"}],"subscriptionAccountsEnabled":false})).await.unwrap();
    assert_eq!(result["models"]["qfmodel"]["price"]["input"], 5.0);
    assert!(!directory.0.join("auth.json").exists());
}
