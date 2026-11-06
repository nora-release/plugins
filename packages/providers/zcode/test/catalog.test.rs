use super::*;
fn config() -> Value {
    json!({"revision":7,"config":{"providerConfigRules":{"providerRules":[{"providerId":"account:zai-individual-coding-plan","builtinModelIds":["REAL-ONE","disabled"]}]},"modelConfigRules":{"builtinProviderModelRules":[{"providerId":"account:zai-individual-coding-plan","modelId":"disabled","config":{"enabled":false}},{"providerId":"account:zai-individual-coding-plan","modelId":"added","config":{"enabled":true}}],"modelRules":[{"modelMatch":"real-.*","config":{"properties":{"contextWindow":32768,"inputFormat":{"supportsImage":true}},"optionSpecs":{"maxOutputTokens":{"max":4096},"reasoningLevel":{"values":["disabled","enabled","high","max"]}}}}]}}})
}
#[test]
fn live_provider_rules_preserve_order_disabled_models_real_limits_and_reasoning_levels() {
    let rows = rules(&config(), Site::Zai, false);
    assert_eq!(
        rows.iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["REAL-ONE", "added"]
    );
    let model = model(&rows[0], Site::Zai, false, "remote");
    assert_eq!(model["contextWindow"], 32768);
    assert_eq!(model["maxTokens"], 4096);
    assert_eq!(
        model["info"]["reasoningEfforts"],
        json!(["none", "high", "max"])
    );
    assert_eq!(model["input"], json!(["text", "image"]));
}
#[test]
fn preset_lists_are_explicit_and_start_has_no_unreported_premium_model() {
    assert_eq!(presets(false).len(), 4);
    assert_eq!(presets(true).len(), 3);
    assert!(!presets(true).iter().any(|row| row["id"] == "GLM-5.3"));
    assert_eq!(
        model(&about("unlisted-real-id"), Site::BigModel, true, "remote")["contextWindow"],
        0
    );
}
