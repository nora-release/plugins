use super::*;

#[test]
fn the_real_redacted_catalog_preserves_models_and_reasoning_levels_without_defaulting_to_xl() {
    let payload: Value =
        serde_json::from_str(include_str!("fixtures/work-models.redacted.json")).unwrap();
    let parsed = parse(&payload).unwrap();
    let models = parsed["models"].as_array().unwrap();
    assert_eq!(models.len(), payload["data"].as_array().unwrap().len());
    for (model, row) in models.iter().zip(payload["data"].as_array().unwrap()) {
        assert_eq!(model["id"], row["id"]);
        assert_eq!(model["contextWindow"], 262144);
        assert_eq!(model["maxTokens"], 32768);
        assert_eq!(model["input"], json!(["text", "image"]));
        assert_eq!(
            model["info"]["reasoningEfforts"],
            json!(["low", "high", "max"])
        );
        assert_eq!(model["baseUrl"], crate::provider::BASE);
    }
    let extended = parse(&json!({"data":[{"id":"future-model","reasoningEffortOptions":[{"effort":"REASONING_EFFORT_STANDARD"},{"effort":"REASONING_EFFORT_ULTRA"}]}]})).unwrap();
    assert_eq!(
        extended["models"][0]["info"]["reasoningEfforts"],
        json!(["standard", "ultra"])
    );
}

#[test]
fn incomplete_empty_and_invalid_catalogs_are_rejected() {
    for payload in [
        json!({}),
        json!({"data":[]}),
        json!({"data":[{"id":"k3-agent"}],"has_more":true}),
        json!({"data":[{"id":"unsafe/model"}]}),
        json!({"data":[{"id":"k3-agent","reasoningEffortOptions":{}}]}),
        json!({"data":[{"id":"k3-agent","reasoningEffortOptions":[{"effort":"INVALID EFFORT"}]}]}),
    ] {
        assert!(parse(&payload).is_err());
    }
}
