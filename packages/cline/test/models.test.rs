use super::*;
#[test]
fn only_named_feed_groups_add_models_and_free_membership_wins_duplicates() {
    let rows=parse(&json!({"recommended":["maker/model",{"id":"other/model","display_name":"Other"}],"free":["maker/model"],"clinePass":["cline-pass/real"],"description":"fake/not-model","clineCloud":["hidden/model"]}),&Value::Null,"remote").unwrap().models;
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["info"]["free"], true);
    assert!(rows[0]["name"].as_str().unwrap().ends_with("(free)"));
    assert!(rows[2]["name"].as_str().unwrap().ends_with("(ClinePass)"));
}
#[test]
fn cloud_catalog_adds_actual_windows_and_only_unanimous_fallback_modalities() {
    let rows=parse(&json!({"recommended":["cline-pass/shared","maker/own"]}),&json!({"data":[{"id":"maker/shared","architecture":{"input_modalities":["text","image"]}},{"id":"maker/own","context_length":123456,"architecture":{"input_modalities":["text","image"]}},{"id":"other/shared","architecture":{"input_modalities":["text"]}}]}),"remote").unwrap().models;
    assert_eq!(rows[0]["input"], json!(["text"]));
    assert_eq!(rows[1]["contextWindow"], 123456);
    assert_eq!(rows[1]["input"], json!(["text", "image"]));
    assert!(rows.iter().all(|row| row["maxTokens"] == 0));
}

#[test]
fn exact_catalog_entries_without_modalities_do_not_borrow_a_makers_capabilities() {
    for input in [Value::Null, json!([])] {
        let catalog = parse(
            &json!({"clinePass": ["cline-pass/deepseek-next"]}),
            &json!([
                {"id": "cline-pass/deepseek-next", "input_modalities": input},
                {"id": "deepseek/deepseek-next", "input_modalities": ["text", "image"]}
            ]),
            "remote",
        )
        .unwrap();
        assert!(!catalog.images.contains_key("cline-pass/deepseek-next"));
    }
}

#[test]
fn fallback_capabilities_compare_known_modalities_not_order_case_or_variants() {
    let catalog = parse(
        &json!({"clinePass": ["cline-pass/deepseek-next", "cline-pass/deepseek-ambiguous"]}),
        &json!([
            {"id": "maker/DeepSeek-Next:batch", "input_modalities": ["TEXT", "IMAGE"]},
            {"id": "other/deepseek-next", "input_modalities": ["image", "text", "image", "unknown"]},
            {"id": "maker/deepseek-ambiguous", "input_modalities": ["text", "image", "audio"]},
            {"id": "other/deepseek-ambiguous", "input_modalities": ["text", "image"]}
        ]),
        "remote",
    ).unwrap();
    assert_eq!(catalog.images.get("cline-pass/deepseek-next"), Some(&true));
    assert!(!catalog.images.contains_key("cline-pass/deepseek-ambiguous"));
}

#[test]
fn presets_preserve_the_official_flash_image_capabilities() {
    let catalog = presets();
    for id in [
        "cline-pass/deepseek-v4.1-flash-1017",
        "cline-pass/deepseek-v4.1-flash",
    ] {
        assert_eq!(catalog.images.get(id), Some(&true));
        let model = catalog
            .models
            .iter()
            .find(|model| model["id"] == id)
            .unwrap();
        assert_eq!(model["input"], json!(["text", "image"]));
    }
    assert_ne!(
        catalog.images.get("cline-pass/deepseek-v4-pro"),
        Some(&true)
    );
}
