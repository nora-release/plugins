use super::*;
#[test]
fn live_catalog_assigns_each_model_its_real_native_api_and_limits() {
    let models = parse(&crate::fixture::models()).unwrap();
    assert_eq!(
        models
            .iter()
            .map(|m| m.value["api"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "anthropic-messages",
            "openai-responses",
            "google-generative-ai",
            "openai-completions"
        ]
    );
    assert_eq!(
        models[0].value["info"]["reasoningEfforts"],
        json!(["low", "high"])
    );
    assert_eq!(models[0].value["contextWindow"], 200000);
    assert_eq!(output(&models)["source"], "remote");
    assert_eq!(output(&models)["allowUnknownModels"], false);
}
#[test]
fn disabled_or_unknown_vendor_models_are_not_offered_as_chat_completions() {
    assert!(parse(&json!({"models":[{"id":"x","provider":"new-vendor"},{"id":"y","provider":"open_ai","is_disabled":true}]})).is_err());
    assert!(parse(&json!({})).is_err());
}
