use super::*;
#[test]
fn full_droid_registry_has_four_actual_protocols_and_honest_source() {
    let output = output(1792627200000);
    assert_eq!(output["source"], "preset");
    assert_eq!(output["models"].as_array().unwrap().len(), 46);
    let mut protocols = std::collections::BTreeSet::new();
    for model in output["models"].as_array().unwrap() {
        protocols.insert(model["api"].as_str().unwrap());
        assert_eq!(model["info"]["catalogSource"], "preset");
        assert_eq!(model["info"]["free"], false);
        assert_eq!(model["info"]["pricingKnown"], false);
        assert!(model["info"]["creditMultiplier"].is_number());
        let source = find(model["id"].as_str().unwrap()).unwrap();
        assert_eq!(model["contextWindow"], source["context"]);
        assert_eq!(model["maxTokens"], source["output"]);
    }
    assert_eq!(protocols.len(), 4);
    let haiku = output["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["id"] == "claude-haiku-4-5-20251001")
        .unwrap();
    // Droid explicitly leaves these limits unknown for the legacy model.
    assert_eq!(haiku["contextWindow"], 0);
    assert_eq!(haiku["maxTokens"], 0);
}
#[test]
fn promotions_change_multiplier_only_inside_the_real_dates() {
    let row = find("gpt-5.6-sol").unwrap();
    let from = chrono::DateTime::parse_from_rfc3339("2026-08-22T00:00:00Z")
        .unwrap()
        .timestamp_millis() as u64;
    let to = chrono::DateTime::parse_from_rfc3339("2026-11-22T00:00:00Z")
        .unwrap()
        .timestamp_millis() as u64;
    assert!(rate(&row, from - 1).1.is_none());
    assert!(rate(&row, from).1.is_some());
    assert!(rate(&row, to).1.is_none());
}
#[test]
fn openai_reasoning_models_declare_temperature_unsupported() {
    for row in output(0)["models"].as_array().unwrap() {
        let source = find(row["id"].as_str().unwrap()).unwrap();
        assert_eq!(row["info"]["temperature"], source["upstream"] != "openai");
    }
}
#[test]
fn standard_and_core_scopes_are_disjoint_complete_and_nonempty() {
    let standard = ids(false);
    let core = ids(true);
    assert!(!standard.is_empty() && !core.is_empty());
    assert_eq!(standard.len() + core.len(), registry().len());
    assert!(!standard.iter().any(|id| core.contains(id)));
    assert!(core.iter().any(|id| id.contains("glm")));
}
